//! Golden tests pinning Ariel's mirrored prospero wire types.
//!
//! The fixtures under `fixtures/prospero/` are written from prospero v0.7.0's
//! wire contract (`crates/types/src/{model,event,api}.rs`, `crates/api/src/sse.rs`).
//! If prospero changes its wire format, these fail before Ariel misreads it.

use ariel_core::prospero::sse::{FrameDecoder, StreamItem};
use ariel_core::prospero::types::{
    AgentStatus, EventKind, FleetEvent, FleetSnapshot, GapSignal, OutputStream, SpawnRequest,
    SpawnedResponse, WorkspaceHealth,
};
use serde_json::{Value, json};

const FLEET: &str = include_str!("fixtures/prospero/fleet_snapshot.json");
const EVENTS: &str = include_str!("fixtures/prospero/events.json");
const STREAM: &str = include_str!("fixtures/prospero/stream.sse");

#[test]
fn fleet_snapshot_decodes() {
    let fleet: FleetSnapshot = serde_json::from_str(FLEET).unwrap();

    assert_eq!(fleet.host, "devbox");
    assert_eq!(fleet.workspaces.len(), 2);
    assert_eq!(fleet.workspaces[0].health, WorkspaceHealth::Healthy);
    assert_eq!(
        fleet.workspaces[1].health,
        WorkspaceHealth::Unreachable {
            reason: "no socket".into()
        }
    );

    let agents: Vec<_> = fleet.agents().collect();
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[0].id, "a1");
    assert_eq!(agents[0].workspace, "caliban");
    assert_eq!(agents[0].status, AgentStatus::Running);
    assert!(!agents[0].status.is_terminal());
    assert!(agents[1].status.is_terminal());
}

#[test]
fn every_event_kind_round_trips() {
    let raw: Vec<Value> = serde_json::from_str(EVENTS).unwrap();
    let events: Vec<FleetEvent> = raw
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();

    for (value, event) in raw.iter().zip(&events) {
        assert_eq!(
            &serde_json::to_value(event).unwrap(),
            value,
            "seq {}",
            event.seq
        );
    }

    let kinds: Vec<&EventKind> = events.iter().map(|e| &e.kind).collect();
    assert!(matches!(kinds[0], EventKind::AgentSpawned));
    assert!(matches!(kinds[1], EventKind::AgentDiscovered));
    assert!(matches!(kinds[2], EventKind::AgentInit { tools, .. } if tools.len() == 2));
    assert!(matches!(
        kinds[3],
        EventKind::StatusChanged {
            from: AgentStatus::Spawning,
            to: AgentStatus::Running
        }
    ));
    assert!(matches!(
        kinds[4],
        EventKind::Output {
            stream: OutputStream::Stdout,
            ..
        }
    ));
    assert!(matches!(
        kinds[5],
        EventKind::Output {
            stream: OutputStream::Thinking,
            ..
        }
    ));
    assert!(
        matches!(kinds[6], EventKind::ToolStarted { input, .. } if input["path"] == "src/lib.rs")
    );
    assert!(matches!(kinds[7], EventKind::ToolFinished { ok: true, .. }));
    assert!(matches!(
        kinds[8],
        EventKind::StorePersistFailed { lost_seq: 5, .. }
    ));
    assert!(matches!(
        kinds[9],
        EventKind::RepoHealth {
            state: WorkspaceHealth::Unreachable { .. }
        }
    ));
    assert!(matches!(
        kinds[10],
        EventKind::AgentFinished { turns: 7, .. }
    ));
    assert!(kinds[10].ends_stream());
    assert!(matches!(kinds[11], EventKind::AgentGone));
}

#[test]
fn unknown_event_kind_is_tolerated() {
    let event: FleetEvent = serde_json::from_value(json!({
        "seq": 4, "ts": "2026-06-18T00:00:00+00:00", "repo": "r", "agent_id": "a1",
        "kind": { "kind": "permission_requested", "tool": "Bash" }
    }))
    .unwrap();

    assert_eq!(event.seq, 4);
    assert_eq!(event.kind, EventKind::Unknown);
}

#[test]
fn unknown_status_and_health_are_tolerated() {
    let status: AgentStatus = serde_json::from_value(json!("paused")).unwrap();
    assert_eq!(status, AgentStatus::Unknown);
    assert!(!status.is_terminal());

    let health: WorkspaceHealth = serde_json::from_value(json!({ "state": "degraded" })).unwrap();
    assert_eq!(health, WorkspaceHealth::Unknown);
}

#[test]
fn terminal_status_change_is_recognised() {
    let done = EventKind::StatusChanged {
        from: AgentStatus::Running,
        to: AgentStatus::Done,
    };
    let idle = EventKind::StatusChanged {
        from: AgentStatus::Running,
        to: AgentStatus::Idle,
    };
    assert!(done.is_terminal_status());
    assert!(!idle.is_terminal_status());
}

#[test]
fn spawn_request_omits_unset_options() {
    assert_eq!(
        serde_json::to_value(SpawnRequest::new("fix the tests")).unwrap(),
        json!({ "prompt": "fix the tests", "interactive": false })
    );
}

#[test]
fn spawned_response_defaults_created_for_older_daemons() {
    let legacy: SpawnedResponse = serde_json::from_value(
        json!({ "agent_id": "a1", "workspace": "caliban", "isolated": true }),
    )
    .unwrap();
    assert!(legacy.created);

    let attached: SpawnedResponse = serde_json::from_value(
        json!({ "agent_id": "a1", "workspace": "caliban", "isolated": true, "created": false }),
    )
    .unwrap();
    assert!(!attached.created);
}

#[test]
fn recorded_stream_decodes_in_awkward_chunks() {
    let mut decoder = FrameDecoder::new();
    let mut items = Vec::new();
    for chunk in STREAM.as_bytes().chunks(7) {
        for frame in decoder.push(chunk) {
            items.extend(StreamItem::from_frame(&frame).unwrap());
        }
    }

    assert_eq!(items.len(), 4);
    assert!(
        matches!(&items[0], StreamItem::Event(e) if e.seq == 1 && e.kind == EventKind::AgentSpawned)
    );
    assert!(
        matches!(&items[1], StreamItem::Event(e) if e.seq == 2 && e.kind == EventKind::Unknown)
    );
    assert_eq!(
        items[2],
        StreamItem::Gap(GapSignal {
            skipped: 7,
            last_seq: 2
        })
    );
    assert!(matches!(&items[3], StreamItem::Event(e) if e.kind.is_terminal_status()));
}
