//! The prospero client and fleet watcher against a stub prosperod.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ariel_core::prospero::sse::StreamItem;
use ariel_core::prospero::types::{EventKind, FleetEvent, GapSignal, SpawnRequest};
use ariel_core::prospero::{ClientError, FleetFrom, FleetWatcher, ProsperoClient, WatchConfig};
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
    /// The body served on each successive fleet-stream connection.
    fleet_scripts: VecDeque<Script>,
    /// The `from` of every fleet-stream connection, in order. A string, since
    /// this endpoint also takes `now`.
    fleet_froms: Vec<String>,
    /// Status to answer the next fleet-stream connection with instead of a
    /// stream, so a connect failure can be scripted.
    fleet_stream_fails: Option<StatusCode>,
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
        .route("/api/fleet/stream", get(fleet_stream_route))
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
    serve_script(script.unwrap_or_else(|| held(vec![]).0))
}

/// `GET /api/fleet/stream` — one connection carrying every agent's events,
/// each with its fleet cursor in the SSE `id:`.
async fn fleet_stream_route(
    State(stub): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let script = {
        let mut stub = stub.lock().unwrap();
        if let Some(status) = stub.fleet_stream_fails.take() {
            return (status, "fleet stream unavailable").into_response();
        }
        stub.fleet_froms
            .push(query.get("from").cloned().unwrap_or_default());
        stub.fleet_scripts.pop_front()
    };
    serve_script(script.unwrap_or_else(|| held(vec![]).0))
}

fn serve_script(script: Script) -> Response {
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

/// One fleet-stream frame: an event carrying its fleet cursor as the SSE `id:`.
fn cursored(cursor: u64, seq: u64, agent: &str, kind: Value) -> String {
    let event = json!({
        "seq": seq, "ts": "2026-06-18T00:00:00+00:00",
        "repo": "caliban", "agent_id": agent, "kind": kind
    });
    format!("id: {cursor}\ndata: {event}\n\n")
}

/// A spawn of `agent` at fleet cursor `cursor`.
fn spawn_at(cursor: u64, agent: &str) -> String {
    cursored(cursor, 0, agent, json!({ "kind": "agent_spawned" }))
}

/// `agent` finishing at fleet cursor `cursor`.
fn finish_at(cursor: u64, agent: &str) -> String {
    cursored(
        cursor,
        1,
        agent,
        json!({ "kind": "agent_finished", "outcome": "success", "cost_usd": 0.1, "turns": 2 }),
    )
}

fn fleet_script(stub: &Shared, script: Script) {
    stub.lock().unwrap().fleet_scripts.push_back(script);
}

fn fleet_froms(stub: &Shared) -> Vec<String> {
    stub.lock().unwrap().fleet_froms.clone()
}

fn fail_fleet_stream_once(stub: &Shared, status: StatusCode) {
    stub.lock().unwrap().fleet_stream_fails = Some(status);
}

fn fast() -> WatchConfig {
    WatchConfig {
        reconnect_delay: Duration::from_millis(20),
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

// ------------------------------------------------------------- fleet stream

#[tokio::test]
async fn fleet_stream_asks_for_only_what_happens_next() {
    let stub = stub();
    let client = start(&stub).await;
    fleet_script(&stub, held(vec![]).0);

    let _stream = client.fleet_stream(FleetFrom::Now).await.unwrap();
    assert_eq!(fleet_froms(&stub), ["now"]);
}

#[tokio::test]
async fn fleet_stream_resumes_after_a_cursor() {
    let stub = stub();
    let client = start(&stub).await;
    fleet_script(&stub, held(vec![]).0);

    let _stream = client.fleet_stream(FleetFrom::After(41)).await.unwrap();
    assert_eq!(fleet_froms(&stub), ["41"]);
}

#[tokio::test]
async fn fleet_stream_yields_each_event_with_its_cursor() {
    let stub = stub();
    fleet_script(
        &stub,
        closes(vec![spawn_at(4821, "a1"), finish_at(4822, "a1")]),
    );
    let client = start(&stub).await;

    let mut stream = std::pin::pin!(client.fleet_stream(FleetFrom::Now).await.unwrap());
    let first = stream.next().await.unwrap().unwrap();
    assert_eq!(first.cursor, Some(4821));
    assert_eq!(first.event.agent_id, "a1");
    let second = stream.next().await.unwrap().unwrap();
    assert_eq!(second.cursor, Some(4822));
    assert!(second.event.kind.ends_stream());
    assert!(stream.next().await.is_none(), "the connection closed");
}

#[tokio::test]
async fn a_refused_fleet_stream_surfaces_before_any_event() {
    let stub = stub();
    fail_fleet_stream_once(&stub, StatusCode::UNAUTHORIZED);
    let client = start(&stub).await;

    assert!(matches!(
        client.fleet_stream(FleetFrom::Now).await,
        Err(ClientError::Auth { .. })
    ));
}

// --------------------------------------------------- the per-agent stream

#[tokio::test]
async fn the_per_agent_stream_still_decodes_events_gaps_and_bad_frames() {
    // `/api/agents/{id}/stream` is no longer what the watcher listens to, but
    // the client still mirrors prosperod's routes one to one (ADR 0005).
    let stub = stub();
    script(
        &stub,
        "a1",
        closes(vec![
            "data: not json\n\n".to_owned(),
            output(0),
            gap(3, 0),
            "event: novel\ndata: {}\n\n".to_owned(),
            finished(1),
        ]),
    );
    let client = start(&stub).await;

    let mut stream = std::pin::pin!(client.stream("a1", 0).await.unwrap());
    assert!(matches!(
        stream.next().await.unwrap(),
        Err(ClientError::Json(_))
    ));
    let StreamItem::Event(first) = stream.next().await.unwrap().unwrap() else {
        panic!("an event")
    };
    assert_eq!(first.seq, 0);
    assert_eq!(
        stream.next().await.unwrap().unwrap(),
        StreamItem::Gap(GapSignal {
            skipped: 3,
            last_seq: 0
        })
    );
    // The `novel` frame is skipped, so the next item is the finish.
    let StreamItem::Event(last) = stream.next().await.unwrap().unwrap() else {
        panic!("an event")
    };
    assert!(last.kind.ends_stream());
    assert_eq!(froms(&stub, "a1"), [0]);
}

// -------------------------------------------------------------------- watcher

#[tokio::test]
async fn startup_asks_for_only_what_happens_next() {
    // ADR 0011: a restarted daemon replays nothing.
    let stub = stub();
    fleet_script(&stub, held(vec![]).0);
    let (_rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    eventually("the fleet stream to open", || {
        !fleet_froms(&stub).is_empty()
    })
    .await;
    assert_eq!(fleet_froms(&stub), ["now"]);
}

#[tokio::test]
async fn an_agent_that_starts_and_finishes_quickly_is_delivered() {
    // The old watcher polled the fleet to discover agents, so an agent that
    // started and finished between two polls was never seen at all. There is
    // no poll now: the stream carries it.
    let stub = stub();
    fleet_script(
        &stub,
        closes(vec![spawn_at(1, "quick"), finish_at(2, "quick")]),
    );
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    let events = until_finished(&mut rx).await;
    assert_eq!(seqs(&events), [0, 1]);
    assert!(events.iter().all(|e| e.agent_id == "quick"));
}

#[tokio::test]
async fn every_agent_arrives_on_one_connection() {
    let stub = stub();
    fleet_script(
        &stub,
        closes(vec![
            spawn_at(1, "a1"),
            spawn_at(2, "a2"),
            finish_at(3, "a2"),
        ]),
    );
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    let events = until_finished(&mut rx).await;
    let agents: Vec<&str> = events.iter().map(|e| e.agent_id.as_str()).collect();
    assert_eq!(agents, ["a1", "a2", "a2"]);
    // One connection for the whole fleet, and no per-agent stream opened.
    assert_eq!(fleet_froms(&stub).len(), 1);
    assert!(froms(&stub, "a1").is_empty());
    assert!(froms(&stub, "a2").is_empty());
}

#[tokio::test]
async fn a_dropped_connection_resumes_after_the_last_cursor() {
    let stub = stub();
    // The first connection delivers two events and then closes.
    fleet_script(&stub, closes(vec![spawn_at(7, "a1"), spawn_at(8, "a2")]));
    fleet_script(&stub, closes(vec![finish_at(9, "a1")]));
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    let events = until_finished(&mut rx).await;
    let agents: Vec<&str> = events.iter().map(|e| e.agent_id.as_str()).collect();
    assert_eq!(agents, ["a1", "a2", "a1"], "no duplicate, nothing skipped");
    assert_eq!(
        fleet_froms(&stub),
        ["now", "8"],
        "resumes strictly after the last cursor it saw"
    );
}

#[tokio::test]
async fn an_undecodable_frame_does_not_end_the_stream() {
    let stub = stub();
    fleet_script(
        &stub,
        closes(vec![
            "id: 1\ndata: not json\n\n".to_owned(),
            "id: 2\nevent: novel\ndata: {}\n\n".to_owned(),
            finish_at(3, "a1"),
        ]),
    );
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    let events = until_finished(&mut rx).await;
    assert_eq!(events.len(), 1, "the bad and the unknown frame are skipped");
    assert!(events[0].kind.ends_stream());
}

#[tokio::test]
async fn an_unknown_event_kind_is_delivered_as_unknown() {
    // ADR 0005: a newer prosperod may add event kinds, and Ariel must keep
    // reading the stream rather than choke on one it does not know.
    let stub = stub();
    fleet_script(
        &stub,
        closes(vec![
            cursored(1, 0, "a1", json!({ "kind": "something_new" })),
            finish_at(2, "a1"),
        ]),
    );
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    let events = until_finished(&mut rx).await;
    assert_eq!(events[0].kind, EventKind::Unknown);
    assert!(events[1].kind.ends_stream());
}

#[tokio::test]
async fn a_refused_connection_is_retried() {
    let stub = stub();
    fail_fleet_stream_once(&stub, StatusCode::INTERNAL_SERVER_ERROR);
    fleet_script(&stub, closes(vec![finish_at(1, "a1")]));
    let (mut rx, _watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);

    assert_eq!(seqs(&until_finished(&mut rx).await), [1]);
}

#[tokio::test]
async fn dropping_the_receiver_stops_the_watcher_and_closes_the_connection() {
    let stub = stub();
    let (connection, closed) = held(vec![]);
    fleet_script(&stub, connection);
    let (rx, watcher) = FleetWatcher::new(start(&stub).await, fast()).spawn(16);
    eventually("the fleet stream to open", || {
        !fleet_froms(&stub).is_empty()
    })
    .await;

    drop(rx);
    tokio::time::timeout(Duration::from_secs(2), watcher)
        .await
        .expect("watcher stops")
        .unwrap();
    eventually("the connection to be dropped", || {
        closed.load(Ordering::SeqCst)
    })
    .await;
}
