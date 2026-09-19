//! Headless end-to-end smoke test (#21): the whole MVP path through real
//! seams.
//!
//! - **gonzalod** is gonzalo's own HTTP service over a filesystem store, with
//!   token authentication on.
//! - **prosperod** is prospero's own API router and fleet manager, with token
//!   authentication on, supervising prospero's `FakeCaliband`: agents spawn,
//!   stream and finish exactly as with caliban, with no model behind them.
//! - **arield** is the real bridge, reaching both over HTTP with its own tokens,
//!   and chatting through the `ConsoleProvider`.
//!
//! One person links their chat account, spawns an agent from chat, watches the
//! notification follow the agent to the end, and asks for the fleet status.
//! Nothing touches the network beyond loopback, and no model API key is needed.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ariel_core::chat::console::{ConsoleProvider, Recorded};
use ariel_core::chat::{Args, ChannelRef, Destination, Message, UserRef, Visibility};
use ariel_core::link::{self, MintRequest};
use ariel_core::notify::NotifyConfig;
use ariel_core::prospero::{ProsperoClient, WatchConfig};
use ariel_core::records::gonzalo::{
    AuditEntry, AuditResult, ChannelConfig, FleetActor, FleetRole, Follows, GrantScope, Identity,
    NotifyPreset,
};
use ariel_core::records::{Records, Write};
use ariel_daemon::bridge::{self, Wiring};
use gonzalo_server::{Auth, Principal, Service};
use gonzalo_store_fs::FsStore;
use prospero_api::auth::{AuthState, SessionKey};
use prospero_core::LocalFleet;
use prospero_core::Scope;
use prospero_core::auth::{TokenSet, generate_token, tokens_file_line};
use prospero_core::discovery::{DiscoveryEnv, EnsureConfig, control_socket_path};
use prospero_core::fleet::{FleetConfig, FleetManager};
use prospero_core::store::JsonlStore;
use prospero_core::testkit::FakeCaliband;
use tempfile::TempDir;

const GONZALO_TOKEN: &str = "gz-e2e-token";
const WORKSPACE: &str = "caliban";
const PROMPT: &str = "fix the flaky test";

/// How long any one step may take before the test gives up.
const STEP: Duration = Duration::from_secs(20);

/// gonzalod over a filesystem store, accepting only [`GONZALO_TOKEN`].
async fn gonzalod(root: &Path) -> String {
    let store = Arc::new(FsStore::new(root));
    let service = Service::new(store.clone(), store);
    let auth = Auth::Enabled(HashMap::from([(
        GONZALO_TOKEN.to_owned(),
        Principal::admin("arield"),
    )]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(gonzalo_server::serve_http(
        listener,
        service,
        Arc::new(auth),
    ));
    url
}

/// prosperod supervising a fake caliban for one workspace, accepting only the
/// returned `operate`-scoped token. The fake must outlive the test.
struct Prosperod {
    url: String,
    token: String,
    fake: FakeCaliband,
    _dirs: [TempDir; 3],
}

async fn prosperod() -> Prosperod {
    let repo = tempfile::tempdir().unwrap();
    let runtime = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let repo_root = repo.path().canonicalize().unwrap();

    let env = DiscoveryEnv {
        caliban_daemon_runtime_dir: Some(runtime.path().to_path_buf()),
        xdg_runtime_dir: None,
        tmpdir: None,
    };
    let fake = FakeCaliband::start_at(&control_socket_path(&repo_root, &env))
        .await
        .unwrap();

    let mut config = FleetConfig::new("e2e-host", data.path());
    config.discovery_env = env;
    config.ensure = EnsureConfig {
        autostart: false,
        ..EnsureConfig::default()
    };
    config.poll_interval = Duration::from_millis(100);
    let store = Arc::new(JsonlStore::open(data.path()).unwrap());
    let manager = FleetManager::new(config, store).await.unwrap();
    manager.add_repo(WORKSPACE, repo_root).await.unwrap();
    tokio::spawn(manager.clone().run());

    let token = generate_token();
    let tokens = TokenSet::parse(&tokens_file_line("ariel", Scope::Operate, &token)).unwrap();
    let local = LocalFleet::new(manager.clone());
    let app = prospero_api::router_with_auth(
        Arc::new(local.clone()),
        Some(Arc::new(local)),
        manager.store(),
        manager.bus(),
        AuthState::enabled(tokens, SessionKey::random(), false),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    Prosperod {
        url,
        token,
        fake,
        _dirs: [repo, runtime, data],
    }
}

fn ops() -> ChannelRef {
    ChannelRef::new("console", "t1", "ops")
}

fn ada() -> UserRef {
    UserRef::new("console", "t1", "ada")
}

/// Wait until `found` picks something out of the console's log.
async fn eventually<T>(
    console: &ConsoleProvider,
    what: &str,
    found: impl Fn(&[Recorded]) -> Option<T>,
) -> T {
    let deadline = Instant::now() + STEP;
    loop {
        let log = console.log();
        if let Some(value) = found(&log) {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {log:#?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The first reply after `from` in the log, with its visibility.
fn reply_after(log: &[Recorded], from: usize) -> Option<(Visibility, Message)> {
    log.iter().skip(from).find_map(|entry| match entry {
        Recorded::Replied {
            visibility,
            message,
            ..
        } => Some((*visibility, message.clone())),
        _ => None,
    })
}

/// Every channel message, posted or edited, whose title names `agent`.
fn agent_messages(log: &[Recorded], agent: &str) -> Vec<Message> {
    log.iter()
        .filter_map(|entry| match entry {
            Recorded::Posted { message, .. } | Recorded::Edited { message, .. } => Some(message),
            _ => None,
        })
        .filter(|message| {
            message
                .title
                .as_deref()
                .is_some_and(|title| title.ends_with(agent))
        })
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn link_spawn_notify_and_status_through_real_prosperod_and_gonzalod() {
    // --- the services -----------------------------------------------------
    let gonzalo_root = tempfile::tempdir().unwrap();
    let gonzalo_url = gonzalod(gonzalo_root.path()).await;
    let prosperod = prosperod().await;

    // An administrator configures #ops to follow the fleet and allow operators,
    // then mints a link token, as `ariel channel set` and `ariel link new` do.
    let admin = Records::connect(&gonzalo_url, GONZALO_TOKEN, Identity::new("ariel-cli")).unwrap();
    let config = ChannelConfig {
        provider: "console".into(),
        tenant: "t1".into(),
        channel: "ops".into(),
        follows: Follows::Fleet,
        notify: NotifyPreset::All,
        ceiling: FleetRole::Operator,
    };
    let written = admin.create(config.key().unwrap(), config).await.unwrap();
    assert!(matches!(written, Write::Committed(_)), "{written:?}");
    let minted = link::mint(
        &admin,
        MintRequest {
            role: FleetRole::Operator,
            scope: GrantScope::Fleet,
            person: None,
            ttl: Duration::from_secs(600),
            minted_by: FleetActor::Service("ariel-cli".into()),
        },
        now_ms(),
    )
    .await
    .unwrap();

    // --- arield -------------------------------------------------------------
    let console = Arc::new(ConsoleProvider::new());
    let wiring = Wiring {
        provider: console.clone(),
        records: Records::connect(&gonzalo_url, GONZALO_TOKEN, Identity::new("arield")).unwrap(),
        prospero: ProsperoClient::new(&prosperod.url)
            .unwrap()
            .with_token(&prosperod.token),
        notify: NotifyConfig {
            hold: Duration::from_millis(100),
            min_edit_interval: Duration::from_millis(100),
            ..NotifyConfig::default()
        },
        watch: WatchConfig {
            poll_interval: Duration::from_millis(200),
            ..WatchConfig::default()
        },
    };
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let bridge = tokio::spawn(bridge::run(wiring, async {
        let _ = stopped.await;
    }));
    eventually(&console, "command registration", |log| {
        log.iter()
            .any(|entry| matches!(entry, Recorded::CommandsRegistered(_)))
            .then_some(())
    })
    .await;
    let here = Destination::Channel(ops());

    // --- 1. link ------------------------------------------------------------
    let before = console.log().len();
    console.inject_command(
        ada(),
        here.clone(),
        "link",
        Args::from_pairs([("token", minted.token.as_str())]),
    );
    let (visibility, reply) =
        eventually(&console, "the link reply", |log| reply_after(log, before)).await;
    assert_eq!(visibility, Visibility::Private);
    assert!(reply.body.contains("Linked"), "{reply:?}");

    // --- 2. spawn -----------------------------------------------------------
    let before = console.log().len();
    console.inject_command(
        ada(),
        here.clone(),
        "spawn",
        Args::from_pairs([("workspace", WORKSPACE), ("prompt", PROMPT)]),
    );
    let (visibility, reply) =
        eventually(&console, "the spawn reply", |log| reply_after(log, before)).await;
    assert_eq!(visibility, Visibility::Public, "{reply:?}");
    let agent = reply
        .body
        .split('`')
        .nth(1)
        .expect("the reply names the agent")
        .to_owned();
    assert!(reply.body.contains(&format!("`{WORKSPACE}`")), "{reply:?}");

    // prosperod handed the prompt to caliban.
    let specs = prosperod.fake.received_specs();
    assert!(
        specs.iter().any(|spec| spec.initial_prompt == PROMPT),
        "caliban never received the prompt: {specs:?}"
    );

    // --- 3. the notification follows the agent to the end ---------------------
    let finished = eventually(&console, "the agent's finished notification", |log| {
        agent_messages(log, &agent)
            .into_iter()
            .find(|message| message.fields.iter().any(|(name, _)| name == "outcome"))
    })
    .await;
    assert_eq!(
        finished.title.as_deref(),
        Some(format!("{WORKSPACE} · {agent}").as_str())
    );

    // --- 4. status ------------------------------------------------------------
    let before = console.log().len();
    console.inject_command(ada(), here.clone(), "status", Args::default());
    let (visibility, reply) =
        eventually(&console, "the status reply", |log| reply_after(log, before)).await;
    assert_eq!(visibility, Visibility::Public, "{reply:?}");
    assert_eq!(reply.title.as_deref(), Some("Fleet status"));
    assert!(
        reply.body.contains(&format!("**{WORKSPACE}**")),
        "{reply:?}"
    );

    let _ = stop.send(());
    bridge.await.unwrap().unwrap();

    // --- the record in gonzalo -------------------------------------------------
    let mut audit = Vec::new();
    for key in admin.list::<AuditEntry>().await.unwrap() {
        audit.push(admin.get::<AuditEntry>(&key).await.unwrap().unwrap().value);
    }
    let actions: Vec<_> = audit.iter().map(|entry| entry.action.as_str()).collect();
    for expected in ["link.mint", "link.redeem", "command.spawn"] {
        assert!(
            actions.contains(&expected),
            "{expected} missing from {actions:?}"
        );
    }
    let spawn = audit
        .iter()
        .find(|entry| entry.action == "command.spawn")
        .unwrap();
    assert_eq!(spawn.result, AuditResult::Succeeded);
    assert_eq!(spawn.target, WORKSPACE);
    assert!(
        matches!(spawn.actor, FleetActor::Person(_)),
        "{:?}",
        spawn.actor
    );

    // The audit trail records the redemption, never the token.
    for entry in &audit {
        assert!(
            !format!("{entry:?}").contains(&minted.token),
            "the link token reached the audit trail"
        );
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}
