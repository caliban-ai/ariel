//! Channel configuration is a record, not a restart (#56).
//!
//! `arield` re-reads the channel records while it runs, so a channel added,
//! retired or re-scoped takes effect on a running daemon. The stub prosperod
//! here hands out a fresh agent on every fleet poll, so "is this channel being
//! served right now?" is answerable at any moment: served channels keep
//! collecting posts, and an unserved one stops.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use ariel_core::chat::Destination;
use ariel_core::chat::console::{ConsoleProvider, Recorded};
use ariel_core::notify::NotifyConfig;
use ariel_core::prospero::{ProsperoClient, WatchConfig};
use ariel_core::records::gonzalo::{
    ChannelConfig, FleetRole, Follows, FsStore, Identity, NotifyPreset,
};
use ariel_core::records::{Records, Write};
use ariel_daemon::bridge::{self, Wiring};
use axum::Router;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;

/// How often the bridge under test re-reads the channel records.
const RELOAD: Duration = Duration::from_millis(200);

/// Long enough for a re-read, a fleet poll, a stream and the notifier's hold.
const SETTLE: Duration = Duration::from_secs(5);

/// A prosperod whose fleet holds one fresh agent per poll, each of which spawns
/// and finishes. A channel that is being notified therefore keeps collecting
/// posts for as long as it is served.
async fn busy_prosperod() -> ProsperoClient {
    let polls = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/api/fleet", get(fleet))
        .route("/api/agents/{id}/stream", get(stream))
        .with_state(polls);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    ProsperoClient::new(&format!("http://{addr}")).unwrap()
}

async fn fleet(State(polls): State<Arc<AtomicUsize>>) -> Response {
    let n = polls.fetch_add(1, Ordering::SeqCst);
    axum::Json(json!({
        "host": "stub",
        "workspaces": [{
            "name": "caliban",
            "health": {"state": "healthy"},
            "agents": [{
                "id": format!("a{n}"),
                "name": format!("a{n}"),
                "workspace": "caliban",
                "status": "spawning",
                "started_at": "2026-09-20T00:00:00Z",
                "isolated": false,
                "interactive": false
            }]
        }]
    }))
    .into_response()
}

async fn stream(Path(id): Path<String>) -> Response {
    let frames = [
        json!({"seq": 0, "ts": "2026-09-20T00:00:00Z", "repo": "caliban", "agent_id": id,
               "kind": {"kind": "agent_spawned"}}),
        json!({"seq": 1, "ts": "2026-09-20T00:00:01Z", "repo": "caliban", "agent_id": id,
               "kind": {"kind": "agent_finished", "outcome": "success", "cost_usd": 0.1,
                        "turns": 1}}),
    ];
    let body: String = frames
        .iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect();
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn config(channel: &str, follows: Follows) -> ChannelConfig {
    ChannelConfig {
        provider: "console".into(),
        tenant: "t1".into(),
        channel: channel.into(),
        follows,
        notify: NotifyPreset::All,
        ceiling: FleetRole::Viewer,
    }
}

async fn create(records: &Records, config: ChannelConfig) {
    let key = config.key().unwrap();
    assert!(matches!(
        records.create(key, config).await.unwrap(),
        Write::Committed(_)
    ));
}

/// Replace a stored channel record, as `ariel channel set` does.
async fn update(records: &Records, config: ChannelConfig) {
    let stored = records
        .get::<ChannelConfig>(&config.key().unwrap())
        .await
        .unwrap()
        .expect("the channel is configured");
    assert!(matches!(
        records.update(&stored, config).await.unwrap(),
        Write::Committed(_)
    ));
}

async fn delete(records: &Records, config: &ChannelConfig) {
    let stored = records
        .get::<ChannelConfig>(&config.key().unwrap())
        .await
        .unwrap()
        .expect("the channel is configured");
    records.delete(&stored).await.unwrap();
}

/// How many messages the console has sent to `channel`.
fn posts(provider: &ConsoleProvider, channel: &str) -> usize {
    provider
        .log()
        .iter()
        .filter_map(|entry| match entry {
            Recorded::Posted { message_ref, .. } | Recorded::Edited { message_ref, .. } => {
                match &message_ref.at {
                    Destination::Channel(at) => Some(at.channel.clone()),
                    Destination::Thread(thread) => Some(thread.channel.channel.clone()),
                }
            }
            _ => None,
        })
        .filter(|sent| sent == channel)
        .count()
}

/// Wait until `channel` has been posted to at least `want` times.
async fn wait_for_posts(provider: &ConsoleProvider, channel: &str, want: usize) -> usize {
    let deadline = Instant::now() + SETTLE;
    loop {
        let seen = posts(provider, channel);
        if seen >= want {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {want} posts to {channel}; saw {seen}: {:?}",
            provider.log()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

struct Running {
    provider: Arc<ConsoleProvider>,
    records: Records,
    stop: tokio::sync::oneshot::Sender<()>,
    bridge: tokio::task::JoinHandle<Result<(), bridge::BridgeError>>,
    _dir: tempfile::TempDir,
}

impl Running {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let records = Records::new(
            Arc::new(FsStore::new(dir.path())),
            Identity::new("arield-test"),
        );
        let provider = Arc::new(ConsoleProvider::new());
        let wiring = Wiring {
            provider: provider.clone(),
            records: records.clone(),
            prospero: busy_prosperod().await,
            notify: NotifyConfig {
                hold: Duration::from_millis(20),
                min_edit_interval: Duration::from_millis(20),
                ..NotifyConfig::default()
            },
            watch: WatchConfig {
                poll_interval: Duration::from_millis(50),
                reconnect_delay: Duration::from_millis(20),
                linger: Duration::from_millis(50),
            },
            channel_reload: RELOAD,
        };
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let bridge = tokio::spawn(async move {
            bridge::run(wiring, async {
                let _ = stopped.await;
            })
            .await
        });
        Self {
            provider,
            records,
            stop,
            bridge,
            _dir: dir,
        }
    }

    async fn finish(self) {
        let _ = self.stop.send(());
        self.bridge.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_added_while_running_is_served_without_a_restart() {
    let running = Running::start().await;

    // Nothing is configured, so nothing is notified, however many agents run.
    tokio::time::sleep(RELOAD * 3).await;
    assert_eq!(posts(&running.provider, "ops"), 0);

    create(&running.records, config("ops", Follows::Fleet)).await;

    wait_for_posts(&running.provider, "ops", 1).await;
    running.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_channel_whose_record_is_deleted_stops_being_notified() {
    let running = Running::start().await;
    let ops = config("ops", Follows::Fleet);
    create(&running.records, ops.clone()).await;
    wait_for_posts(&running.provider, "ops", 1).await;

    delete(&running.records, &ops).await;
    // Past the re-read, then past enough polls to have posted again if served.
    tokio::time::sleep(RELOAD * 2).await;
    let settled = posts(&running.provider, "ops");
    tokio::time::sleep(RELOAD * 3).await;

    assert_eq!(
        posts(&running.provider, "ops"),
        settled,
        "a channel with no record was still notified"
    );
    running.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_follows_takes_effect_without_a_restart() {
    let running = Running::start().await;
    // Following a workspace the stub's agents are not in: nothing is heard.
    create(
        &running.records,
        config("ops", Follows::workspaces(["elsewhere"]).unwrap()),
    )
    .await;
    tokio::time::sleep(RELOAD * 3).await;
    assert_eq!(posts(&running.provider, "ops"), 0);

    update(&running.records, config("ops", Follows::Fleet)).await;

    wait_for_posts(&running.provider, "ops", 1).await;
    running.finish().await;
}
