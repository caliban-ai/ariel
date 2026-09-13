//! The prospero client and fleet watcher against a stub prosperod.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ariel_core::prospero::types::{EventKind, FleetEvent, SpawnRequest};
use ariel_core::prospero::{ClientError, FleetWatcher, ProsperoClient, WatchConfig};
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::{Stream, StreamExt, stream};
use serde_json::{Value, json};
use tokio::sync::mpsc::Receiver;

// ---------------------------------------------------------------- stub server

type Shared = Arc<Mutex<Stub>>;

#[derive(Default)]
struct Stub {
    /// `None` makes `GET /api/fleet` fail.
    fleet: Option<Value>,
    /// Per agent, the body served on each successive stream connection. Once
    /// exhausted, a connection gets an empty stream held open.
    scripts: HashMap<String, VecDeque<Script>>,
    /// Per agent, the `from` of every stream connection, in order.
    froms: HashMap<String, Vec<u64>>,
    /// Every other request, as `METHOD path body`.
    calls: Vec<String>,
}

struct Script {
    frames: Vec<String>,
    hold_open: bool,
    /// Set once the server has dropped this connection's body.
    closed: Arc<AtomicBool>,
}

struct CloseFlag(Arc<AtomicBool>);

impl Drop for CloseFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn start(stub: &Shared) -> ProsperoClient {
    let app = Router::new()
        .route("/api/agents/{id}/stream", get(stream_route))
        .fallback(other_route)
        .with_state(stub.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    ProsperoClient::new(&format!("http://{addr}")).unwrap()
}

async fn stream_route(
    State(stub): State<Shared>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let from = query.get("from").map_or(0, |f| f.parse().unwrap());
    let script = {
        let mut stub = stub.lock().unwrap();
        stub.froms.entry(id.clone()).or_default().push(from);
        stub.scripts.get_mut(&id).and_then(VecDeque::pop_front)
    };
    let script = script.unwrap_or_else(|| held(vec![]).0);

    let flag = CloseFlag(script.closed);
    let frames = stream::iter(
        script
            .frames
            .into_iter()
            .map(|frame| Ok::<_, Infallible>(Bytes::from(frame))),
    );
    let body: Pin<Box<dyn Stream<Item = Result<Bytes, Infallible>> + Send>> = if script.hold_open {
        Box::pin(frames.chain(stream::pending()))
    } else {
        Box::pin(frames)
    };
    let body = body.map(move |chunk| {
        let _flag = &flag;
        chunk
    });
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(body),
    )
        .into_response()
}

async fn other_route(
    State(stub): State<Shared>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let mut stub = stub.lock().unwrap();
    let path = uri.path();
    stub.calls.push(
        format!("{method} {path} {}", String::from_utf8_lossy(&body))
            .trim_end()
            .to_owned(),
    );
    match (method.as_str(), path) {
        ("GET", "/api/fleet") => match &stub.fleet {
            Some(fleet) => Json(fleet.clone()).into_response(),
            None => (StatusCode::INTERNAL_SERVER_ERROR, "fleet unavailable").into_response(),
        },
        ("POST", "/api/workspaces/caliban/agents") => (
            StatusCode::CREATED,
            Json(json!({ "agent_id": "a9", "workspace": "caliban", "isolated": true })),
        )
            .into_response(),
        ("POST", "/api/agents/a9/kill" | "/api/agents/a9/input" | "/api/agents/a9/end-input") => {
            StatusCode::ACCEPTED.into_response()
        }
        ("POST", "/api/agents/a9/respawn") => Json(json!({ "agent_id": "a10" })).into_response(),
        ("POST", "/api/agents/a%20b/kill") => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "agent not found", "kind": "not_found" })),
        )
            .into_response(),
        _ => (StatusCode::BAD_GATEWAY, "upstream exploded").into_response(),
    }
}

// ------------------------------------------------------------------- fixtures

fn stub() -> Shared {
    Arc::default()
}

fn fleet(agents: &[(&str, &str)]) -> Value {
    let agents: Vec<Value> = agents
        .iter()
        .map(|(id, status)| {
            json!({
                "id": id, "name": id, "workspace": "caliban", "status": status,
                "started_at": "2026-06-18T00:00:00+00:00", "isolated": true,
                "interactive": false, "session_dir": "/tmp/s"
            })
        })
        .collect();
    json!({
        "host": "stub",
        "workspaces": [{
            "name": "caliban", "root": "/src/caliban",
            "health": { "state": "healthy" }, "agents": agents
        }]
    })
}

fn event(seq: u64, kind: Value) -> String {
    let event = json!({
        "seq": seq, "ts": "2026-06-18T00:00:00+00:00",
        "repo": "caliban", "agent_id": "a1", "kind": kind
    });
    format!("data: {event}\n\n")
}

fn output(seq: u64) -> String {
    event(
        seq,
        json!({ "kind": "output", "stream": "stdout", "chunk": format!("line {seq}") }),
    )
}

fn finished(seq: u64) -> String {
    event(
        seq,
        json!({ "kind": "agent_finished", "outcome": "success", "cost_usd": 0.1, "turns": 2 }),
    )
}

fn gap(skipped: u64, last_seq: u64) -> String {
    format!(
        "event: gap\ndata: {}\n\n",
        json!({ "skipped": skipped, "last_seq": last_seq })
    )
}

/// A connection that sends `frames` and then closes.
fn closes(frames: Vec<String>) -> Script {
    Script {
        frames,
        hold_open: false,
        closed: Arc::default(),
    }
}

/// A connection that sends `frames` and then stays open, with a flag set when
/// the connection is dropped.
fn held(frames: Vec<String>) -> (Script, Arc<AtomicBool>) {
    let closed = Arc::new(AtomicBool::new(false));
    let script = Script {
        frames,
        hold_open: true,
        closed: closed.clone(),
    };
    (script, closed)
}

fn script(stub: &Shared, agent: &str, script: Script) {
    stub.lock()
        .unwrap()
        .scripts
        .entry(agent.to_owned())
        .or_default()
        .push_back(script);
}

fn set_fleet(stub: &Shared, agents: &[(&str, &str)]) {
    stub.lock().unwrap().fleet = Some(fleet(agents));
}

fn froms(stub: &Shared, agent: &str) -> Vec<u64> {
    stub.lock()
        .unwrap()
        .froms
        .get(agent)
        .cloned()
        .unwrap_or_default()
}

fn fast() -> WatchConfig {
    WatchConfig {
        poll_interval: Duration::from_millis(50),
        reconnect_delay: Duration::from_millis(20),
        linger: Duration::from_millis(100),
    }
}

async fn until_finished(rx: &mut Receiver<FleetEvent>) -> Vec<FleetEvent> {
    let mut events = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = rx.recv().await {
            let ends = event.kind.ends_stream();
            events.push(event);
            if ends {
                break;
            }
        }
    })
    .await
    .expect("agent_finished within 5s");
    events
}

fn seqs(events: &[FleetEvent]) -> Vec<u64> {
    events.iter().map(|e| e.seq).collect()
}

async fn eventually(what: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// --------------------------------------------------------------------- client

#[tokio::test]
async fn client_speaks_prospero_routes() {
    let stub = stub();
    let client = start(&stub).await;

    let spawned = client
        .spawn("caliban", &SpawnRequest::new("fix the tests"))
        .await
        .unwrap();
    assert_eq!(spawned.agent_id, "a9");
    assert!(spawned.created);
    client.kill("a9").await.unwrap();
    assert_eq!(client.respawn("a9").await.unwrap().agent_id, "a10");
    client.input("a9", "keep going").await.unwrap();
    client.end_input("a9").await.unwrap();

    assert_eq!(
        stub.lock().unwrap().calls,
        [
            r#"POST /api/workspaces/caliban/agents {"prompt":"fix the tests","interactive":false}"#,
            "POST /api/agents/a9/kill",
            "POST /api/agents/a9/respawn",
            r#"POST /api/agents/a9/input {"text":"keep going"}"#,
            "POST /api/agents/a9/end-input",
        ]
    );
}

#[tokio::test]
async fn api_errors_carry_status_and_kind() {
    let stub = stub();
    let client = start(&stub).await;

    match client.kill("a b").await {
        Err(ClientError::Api {
            status,
            kind,
            message,
        }) => {
            assert_eq!(status.as_u16(), 404);
            assert_eq!(kind.as_deref(), Some("not_found"));
            assert_eq!(message, "agent not found");
        }
        other => panic!("expected a not_found API error, got {other:?}"),
    }

    let error = client
        .spawn("elsewhere", &SpawnRequest::new("x"))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ClientError::Api { kind: None, message, .. } if message == "upstream exploded")
    );
    assert!(error.to_string().starts_with("prospero returned 502"));
}

#[tokio::test]
async fn fleet_is_fetched_and_failures_surface() {
    let stub = stub();
    let client = start(&stub).await;

    assert!(matches!(
        client.fleet().await,
        Err(ClientError::Api { status, .. }) if status.as_u16() == 500
    ));

    set_fleet(&stub, &[("a1", "running")]);
    let fleet = client.fleet().await.unwrap();
    assert_eq!(fleet.agents().next().unwrap().id, "a1");
}

#[tokio::test]
async fn an_unreachable_prosperod_is_a_transport_error() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let client = ProsperoClient::new(&format!("http://{addr}")).unwrap();
    assert!(matches!(
        client.fleet().await,
        Err(ClientError::Transport(_))
    ));
}

// -------------------------------------------------------------------- watcher

#[tokio::test]
async fn new_agent_is_discovered_within_one_poll_interval() {
    let stub = stub();
    let config = WatchConfig {
        poll_interval: Duration::from_millis(200),
        ..fast()
    };
    set_fleet(&stub, &[]);
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, config).spawn(16);
    tokio::time::sleep(Duration::from_millis(250)).await;

    script(
        &stub,
        "a1",
        held(vec![event(0, json!({ "kind": "agent_spawned" }))]).0,
    );
    set_fleet(&stub, &[("a1", "running")]);
    let added = Instant::now();

    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("an event")
        .expect("watcher running");
    assert_eq!(first.kind, EventKind::AgentSpawned);
    assert!(
        added.elapsed() < config.poll_interval + Duration::from_millis(300),
        "discovered after {:?}",
        added.elapsed()
    );
}

#[tokio::test]
async fn gap_frame_keeps_position() {
    let stub = stub();
    script(
        &stub,
        "a1",
        closes(vec![
            output(0),
            output(1),
            gap(3, 1),
            output(2),
            output(3),
            output(4),
            finished(5),
        ]),
    );
    set_fleet(&stub, &[("a1", "running")]);
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    assert_eq!(seqs(&until_finished(&mut rx).await), [0, 1, 2, 3, 4, 5]);
    assert_eq!(froms(&stub, "a1"), [0]);
}

#[tokio::test]
async fn reconnect_resumes_without_duplicate_delivery() {
    let stub = stub();
    // The first connection drops after seq 2. The second replays from further
    // back than asked, which the watcher must not deliver twice.
    script(&stub, "a1", closes(vec![output(0), output(1), output(2)]));
    script(
        &stub,
        "a1",
        closes(vec![
            output(1),
            output(2),
            output(3),
            output(4),
            finished(5),
        ]),
    );
    set_fleet(&stub, &[("a1", "running")]);
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    assert_eq!(seqs(&until_finished(&mut rx).await), [0, 1, 2, 3, 4, 5]);
    assert_eq!(froms(&stub, "a1"), [0, 3]);
}

#[tokio::test]
async fn unknown_event_kinds_and_undecodable_frames_are_tolerated() {
    let stub = stub();
    script(
        &stub,
        "a1",
        closes(vec![
            "data: not json\n\n".to_owned(),
            event(0, json!({ "kind": "permission_requested", "tool": "Bash" })),
            "event: novel\ndata: {}\n\n".to_owned(),
            finished(1),
        ]),
    );
    set_fleet(&stub, &[("a1", "running")]);
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    let events = until_finished(&mut rx).await;
    assert_eq!(seqs(&events), [0, 1]);
    assert_eq!(events[0].kind, EventKind::Unknown);
    assert_eq!(froms(&stub, "a1"), [0]);
}

#[tokio::test]
async fn terminal_status_hangs_up_after_linger() {
    let stub = stub();
    let (connection, closed) = held(vec![event(
        0,
        json!({ "kind": "status_changed", "from": "running", "to": "killed" }),
    )]);
    script(&stub, "a1", connection);
    set_fleet(&stub, &[("a1", "running")]);
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(event.kind.is_terminal_status());
    eventually("the stream to be dropped", || closed.load(Ordering::SeqCst)).await;

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(froms(&stub, "a1"), [0], "no reconnect after hanging up");
}

#[tokio::test]
async fn history_is_skipped_at_startup_but_quick_agents_are_not() {
    let stub = stub();
    set_fleet(&stub, &[("a2", "done")]);
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(froms(&stub, "a2").is_empty());

    // a3 started and finished between two polls.
    script(
        &stub,
        "a3",
        closes(vec![
            event(0, json!({ "kind": "agent_spawned" })),
            finished(1),
        ]),
    );
    set_fleet(&stub, &[("a2", "done"), ("a3", "done")]);

    assert_eq!(seqs(&until_finished(&mut rx).await), [0, 1]);
    assert!(froms(&stub, "a2").is_empty());
}

#[tokio::test]
async fn vanished_agent_stream_is_closed() {
    let stub = stub();
    let (connection, closed) = held(vec![]);
    script(&stub, "a1", connection);
    set_fleet(&stub, &[("a1", "running")]);
    let (_rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);
    eventually("the stream to open", || froms(&stub, "a1") == [0]).await;

    set_fleet(&stub, &[]);
    eventually("the stream to be dropped", || closed.load(Ordering::SeqCst)).await;
}

#[tokio::test]
async fn fleet_poll_failures_are_retried() {
    let stub = stub();
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);
    tokio::time::sleep(Duration::from_millis(120)).await;

    script(&stub, "a1", closes(vec![finished(0)]));
    set_fleet(&stub, &[("a1", "running")]);

    assert_eq!(seqs(&until_finished(&mut rx).await), [0]);
}

#[tokio::test]
async fn dropping_the_receiver_stops_the_watcher() {
    let stub = stub();
    set_fleet(&stub, &[]);
    let (rx, watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    drop(rx);
    tokio::time::timeout(Duration::from_secs(2), watcher)
        .await
        .expect("watcher stops")
        .unwrap();
}
