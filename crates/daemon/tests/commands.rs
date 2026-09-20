//! Chat commands reaching `arield` (#16): `/ariel link` through the bridge.

use std::sync::Arc;
use std::time::Duration;

use ariel_core::chat::console::{ConsoleProvider, Recorded};
use ariel_core::chat::{Args, ChannelRef, Destination, UserRef, Visibility};
use ariel_core::link::{self, MintRequest};
use ariel_core::notify::NotifyConfig;
use ariel_core::prospero::{ProsperoClient, WatchConfig};
use ariel_core::records::Records;
use ariel_core::records::gonzalo::{
    Authenticator, FleetActor, FleetRole, FsStore, GrantScope, Identity, IdentityBinding,
};
use ariel_daemon::bridge::{self, Wiring};
use axum::Router;
use axum::routing::get;
use serde_json::json;

/// A prosperod with an empty fleet: these tests are about commands only.
async fn empty_prosperod() -> ProsperoClient {
    let app = Router::new().route(
        "/api/fleet",
        get(|| async { axum::Json(json!({"host": "stub", "workspaces": []})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    ProsperoClient::new(&format!("http://{addr}")).unwrap()
}

/// Run the bridge until the returned sender is used or dropped.
async fn start(
    provider: Arc<ConsoleProvider>,
    records: Records,
) -> (
    tokio::sync::oneshot::Sender<()>,
    tokio::task::JoinHandle<Result<(), bridge::BridgeError>>,
) {
    let console = provider.clone();
    let wiring = Wiring {
        provider,
        records,
        prospero: empty_prosperod().await,
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
    // Wait for the bridge to register its commands, which is the point it is
    // listening. Polled rather than slept: a fixed wait passes on a quiet
    // machine and flakes on a loaded CI runner.
    eventually("command registration", || {
        console
            .log()
            .iter()
            .any(|entry| matches!(entry, Recorded::CommandsRegistered(_)))
    })
    .await;
    (stop, bridge)
}

/// How long a test waits for the bridge to do something before giving up.
const STEP: Duration = Duration::from_secs(10);

/// Poll `done` until it holds, or fail the test.
async fn eventually(what: &str, done: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + STEP;
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Wait for the bridge to answer a command privately.
async fn wait_for_private_reply(provider: &ConsoleProvider) {
    eventually("a private reply", || {
        !private_replies(&provider.log()).is_empty()
    })
    .await;
}

fn store() -> (tempfile::TempDir, Records) {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::new(
        Arc::new(FsStore::new(dir.path())),
        Identity::new("arield-test"),
    );
    (dir, records)
}

fn here() -> Destination {
    Destination::Channel(ChannelRef::new("console", "t1", "general"))
}

/// Private replies the console recorded, as text.
fn private_replies(log: &[Recorded]) -> Vec<String> {
    log.iter()
        .filter_map(|entry| match entry {
            Recorded::Replied {
                visibility: Visibility::Private,
                message,
                ..
            } => Some(message.body.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn the_bridge_registers_every_command() {
    let (_dir, records) = store();
    let provider = Arc::new(ConsoleProvider::new());
    let (stop, bridge) = start(provider.clone(), records).await;
    let _ = stop.send(());
    bridge.await.unwrap().unwrap();

    let registered = provider.log().iter().any(|entry| {
        matches!(entry, Recorded::CommandsRegistered(names) if names == &["link", "status", "spawn", "kill", "respawn"])
    });
    assert!(registered, "{:?}", provider.log());
}

#[tokio::test]
async fn ariel_status_reaches_the_command_router() {
    let (_dir, records) = store();
    let provider = Arc::new(ConsoleProvider::new());
    let (stop, bridge) = start(provider.clone(), records).await;

    // No channel configuration: the router answers with the fix, privately.
    provider.inject_command(
        UserRef::new("console", "t1", "user-7"),
        here(),
        "status",
        Args::default(),
    );
    wait_for_private_reply(&provider).await;
    let _ = stop.send(());
    bridge.await.unwrap().unwrap();

    let replies = private_replies(&provider.log());
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(replies[0].contains("ariel channel set"), "{replies:?}");
}

#[tokio::test]
async fn ariel_link_in_chat_links_the_account_and_answers_privately() {
    let (_dir, records) = store();
    let minted = link::mint(
        &records,
        MintRequest {
            role: FleetRole::Operator,
            scope: GrantScope::Fleet,
            person: None,
            ttl: Duration::from_secs(600),
            minted_by: FleetActor::Service("ariel-cli".into()),
        },
        // Minted now, so it is still valid when the bridge redeems it.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
    )
    .await
    .unwrap();

    let provider = Arc::new(ConsoleProvider::new());
    let (stop, bridge) = start(provider.clone(), records.clone()).await;

    provider.inject_command(
        UserRef::new("console", "t1", "user-7"),
        here(),
        "link",
        Args::from_pairs([("token", minted.token.as_str())]),
    );
    wait_for_private_reply(&provider).await;
    let _ = stop.send(());
    bridge.await.unwrap().unwrap();

    let log = provider.log();
    let replies = private_replies(&log);
    assert_eq!(replies.len(), 1, "exactly one private answer: {log:?}");
    assert!(replies[0].contains("Linked"), "{replies:?}");
    assert!(replies[0].contains("operator"), "{replies:?}");
    assert!(
        !replies[0].contains(&minted.token),
        "the reply must not repeat the token"
    );
    assert!(
        !log.iter().any(|entry| matches!(
            entry,
            Recorded::Replied {
                visibility: Visibility::Public,
                ..
            }
        )),
        "nothing about linking is said in public: {log:?}"
    );

    let binding = records
        .get::<IdentityBinding>(
            &IdentityBinding::key_for(&Authenticator::Other("console".into()), "user-7").unwrap(),
        )
        .await
        .unwrap();
    assert!(binding.is_some(), "the account was bound");
}

#[tokio::test]
async fn a_bad_token_is_refused_privately_without_echoing_it() {
    let (_dir, records) = store();
    let provider = Arc::new(ConsoleProvider::new());
    let (stop, bridge) = start(provider.clone(), records).await;

    let guess = "0123456789abcdef".repeat(4);
    provider.inject_command(
        UserRef::new("console", "t1", "user-8"),
        here(),
        "link",
        Args::from_pairs([("token", guess.as_str())]),
    );
    wait_for_private_reply(&provider).await;
    let _ = stop.send(());
    bridge.await.unwrap().unwrap();

    let replies = private_replies(&provider.log());
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(replies[0].contains("Not linked"), "{replies:?}");
    assert!(!replies[0].contains(&guess), "the guess is not echoed");
}

#[tokio::test]
async fn an_unknown_command_gets_a_private_reply() {
    let (_dir, records) = store();
    let provider = Arc::new(ConsoleProvider::new());
    let (stop, bridge) = start(provider.clone(), records).await;

    provider.inject_command(
        UserRef::new("console", "t1", "user-9"),
        here(),
        "dance",
        Args::default(),
    );
    wait_for_private_reply(&provider).await;
    let _ = stop.send(());
    bridge.await.unwrap().unwrap();

    let replies = private_replies(&provider.log());
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert!(
        replies[0].contains("not a command Ariel knows"),
        "{replies:?}"
    );
}
