//! `arield` wired end to end (#19): a stub prosperod, channel configuration in
//! gonzalo, and a console chat provider.
//!
//! The daemon reads channel records, watches the fleet, and notifies each
//! channel that follows the workspace an event came from.

use std::sync::Arc;
use std::time::Duration;

use ariel_core::chat::console::{ConsoleProvider, Recorded};
use ariel_core::chat::{ChatProvider, Destination};
use ariel_core::notify::NotifyConfig;
use ariel_core::prospero::{ProsperoClient, WatchConfig};
use ariel_core::records::gonzalo::{
    ChannelConfig, FleetRole, Follows, FsStore, Identity, NotifyPreset,
};
use ariel_core::records::{Records, Write};
use ariel_daemon::bridge::{self, Wiring};
use axum::Router;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;

/// A prosperod with one workspace, one agent, and a stream that spawns then
/// finishes it.
async fn stub_prosperod() -> ProsperoClient {
    let app = Router::new()
        .route(
            "/api/fleet",
            get(|| async {
                axum::Json(json!({
                    "host": "stub",
                    "workspaces": [{
                        "name": "caliban",
                        "health": {"state": "healthy"},
                        "agents": [{
                            "id": "a1",
                            "name": "worker",
                            "workspace": "caliban",
                            "status": "spawning",
                            "started_at": "2026-09-15T00:00:00Z",
                            "isolated": false,
                            "interactive": false
                        }]
                    }]
                }))
            }),
        )
        .route("/api/agents/{id}/stream", get(stream_route));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    ProsperoClient::new(&format!("http://{addr}")).unwrap()
}

async fn stream_route() -> Response {
    let frames = [
        json!({"seq": 0, "ts": "2026-09-15T00:00:00Z", "repo": "caliban", "agent_id": "a1",
               "kind": {"kind": "agent_spawned"}}),
        json!({"seq": 1, "ts": "2026-09-15T00:00:01Z", "repo": "caliban", "agent_id": "a1",
               "kind": {"kind": "status_changed", "from": "spawning", "to": "running"}}),
        json!({"seq": 2, "ts": "2026-09-15T00:00:09Z", "repo": "caliban", "agent_id": "a1",
               "kind": {"kind": "agent_finished", "outcome": "success", "cost_usd": 0.5,
                        "turns": 4}}),
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

fn channel_config(channel: &str, follows: Follows, notify: NotifyPreset) -> ChannelConfig {
    ChannelConfig {
        provider: "console".into(),
        tenant: "t1".into(),
        channel: channel.into(),
        follows,
        notify,
        ceiling: FleetRole::Viewer,
    }
}

async fn store_channel(records: &Records, config: ChannelConfig) {
    let key = config.key().unwrap();
    assert!(matches!(
        records.create(key, config).await.unwrap(),
        Write::Committed(_)
    ));
}

/// Which channel each recorded send went to.
fn channels(log: &[Recorded]) -> Vec<String> {
    log.iter()
        .filter_map(|entry| match entry {
            Recorded::Posted { message_ref, .. } | Recorded::Edited { message_ref, .. } => {
                match &message_ref.at {
                    Destination::Channel(channel) => Some(channel.channel.clone()),
                    Destination::Thread(thread) => Some(thread.channel.channel.clone()),
                }
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_agent_notifies_only_the_channels_following_its_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::new(
        Arc::new(FsStore::new(dir.path())),
        Identity::new("arield-test"),
    );
    store_channel(
        &records,
        channel_config(
            "ops",
            Follows::workspaces(["caliban"]).unwrap(),
            NotifyPreset::All,
        ),
    )
    .await;
    store_channel(
        &records,
        channel_config(
            "elsewhere",
            Follows::workspaces(["prospero"]).unwrap(),
            NotifyPreset::All,
        ),
    )
    .await;

    let provider = Arc::new(ConsoleProvider::new());
    let wiring = Wiring {
        provider: provider.clone(),
        records,
        prospero: stub_prosperod().await,
        notify: NotifyConfig {
            hold: Duration::from_millis(50),
            min_edit_interval: Duration::from_millis(50),
            ..NotifyConfig::default()
        },
        watch: WatchConfig {
            poll_interval: Duration::from_millis(20),
            reconnect_delay: Duration::from_millis(20),
            linger: Duration::from_millis(50),
        },
        channel_reload: bridge::CHANNEL_RELOAD,
    };

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let bridge = tokio::spawn(async move {
        bridge::run(wiring, async {
            let _ = stopped.await;
        })
        .await
    });

    // Long enough for the fleet poll, the stream and the notifier's hold.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = stop.send(());
    bridge.await.unwrap().unwrap();

    let log = provider.log();
    let channels = channels(&log);
    assert!(
        !channels.is_empty(),
        "the followed channel heard nothing: {log:?}"
    );
    assert!(
        channels.iter().all(|channel| channel == "ops"),
        "a channel that does not follow caliban was notified: {channels:?}"
    );

    let last = log.last().expect("at least one send");
    let message = match last {
        Recorded::Posted { message, .. } | Recorded::Edited { message, .. } => message,
        other => panic!("unexpected send: {other:?}"),
    };
    assert_eq!(message.title.as_deref(), Some("caliban · a1"));
    assert!(
        message.fields.iter().any(|(name, _)| name == "outcome"),
        "the agent's end reached the channel: {message:?}"
    );
}

#[tokio::test]
async fn channels_for_other_providers_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::new(
        Arc::new(FsStore::new(dir.path())),
        Identity::new("arield-test"),
    );
    let mut discord = channel_config("d1", Follows::Fleet, NotifyPreset::All);
    discord.provider = "discord".into();
    store_channel(&records, discord).await;

    let provider = Arc::new(ConsoleProvider::new());
    let routes = bridge::routes_for(&records, &provider.id()).await.unwrap();
    assert!(
        routes.is_empty(),
        "a discord channel is not this provider's to serve: {routes:?}"
    );
}

#[tokio::test]
async fn every_channel_of_this_provider_becomes_a_route() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::new(
        Arc::new(FsStore::new(dir.path())),
        Identity::new("arield-test"),
    );
    store_channel(
        &records,
        channel_config("ops", Follows::Fleet, NotifyPreset::All),
    )
    .await;
    store_channel(
        &records,
        channel_config("alerts", Follows::Fleet, NotifyPreset::Failures),
    )
    .await;

    let provider = Arc::new(ConsoleProvider::new());
    let mut routes = bridge::routes_for(&records, &provider.id()).await.unwrap();
    routes.sort_by_key(|route| route.channel().channel.clone());

    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].channel().channel, "alerts");
    assert_eq!(routes[0].notify(), NotifyPreset::Failures);
    assert_eq!(routes[1].channel().channel, "ops");
}

#[tokio::test]
async fn a_bridge_with_no_configured_channels_still_runs_and_stops() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::new(
        Arc::new(FsStore::new(dir.path())),
        Identity::new("arield-test"),
    );
    let wiring = Wiring {
        provider: Arc::new(ConsoleProvider::new()),
        records,
        prospero: stub_prosperod().await,
        notify: NotifyConfig::default(),
        watch: WatchConfig::default(),
        channel_reload: bridge::CHANNEL_RELOAD,
    };

    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let bridge = tokio::spawn(async move {
        bridge::run(wiring, async {
            let _ = stopped.await;
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = stop.send(());
    bridge.await.unwrap().unwrap();
}
