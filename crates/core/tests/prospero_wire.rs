//! Golden tests pinning Ariel's mirrored prospero wire types.
//!
//! The fixtures under `fixtures/prospero/` are written from prospero v0.8.1's
//! wire contract (`crates/types/src/{model,event,api,auth}.rs`, `crates/api/src/sse/`).
//! v0.8 added only the optional `actor` on the event envelope and the
//! `/api/session` route; every other shape is unchanged from v0.7.0.
//! If prospero changes its wire format, these fail before Ariel misreads it.

use ariel_core::prospero::sse::{FrameDecoder, StreamItem};
use ariel_core::prospero::types::{
    AgentStatus, EventKind, FleetEvent, FleetSnapshot, GapSignal, OutputStream, Scope, SessionInfo,
    SpawnRequest, SpawnedResponse, WorkspaceHealth,
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
    assert!(
        matches!(&items[0], StreamItem::Event(e) if e.actor.as_deref() == Some("ariel")),
        "the recorded stream carries v0.8's actor"
    );
}

#[test]
fn an_event_names_the_token_that_caused_it_when_prospero_knows() {
    let events: Vec<FleetEvent> = serde_json::from_str(EVENTS).unwrap();

    // prospero v0.8 attributes an API spawn and removal to the token that made
    // them; every other event carries no actor.
    assert_eq!(events[0].actor.as_deref(), Some("ariel"));
    assert_eq!(events[11].actor.as_deref(), Some("ariel"));
    assert!(
        events[1..11].iter().all(|event| event.actor.is_none()),
        "only the spawn and removal are attributed"
    );
}

#[test]
fn an_event_without_an_actor_serializes_without_one() {
    let event: FleetEvent = serde_json::from_value(json!({
        "seq": 3, "ts": "t", "repo": "caliban", "agent_id": "a1",
        "kind": { "kind": "agent_spawned" }
    }))
    .unwrap();
    assert!(event.actor.is_none(), "a v0.7 daemon's events still decode");
    assert!(
        serde_json::to_value(&event).unwrap().get("actor").is_none(),
        "absent on the wire, as prospero writes it"
    );
}

#[test]
fn session_info_decodes_both_shapes() {
    let disabled: SessionInfo = serde_json::from_value(json!({"auth": "disabled"})).unwrap();
    assert_eq!(disabled, SessionInfo::Disabled);

    let token: SessionInfo =
        serde_json::from_value(json!({"auth": "token", "token_name": "ariel", "scope": "operate"}))
            .unwrap();
    assert_eq!(
        token,
        SessionInfo::Token {
            token_name: "ariel".into(),
            scope: Scope::Operate,
            expires_at: None,
        }
    );
}
