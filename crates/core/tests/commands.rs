//! `/ariel status` and `/ariel spawn` (#20): authorized on two keys, answered in
//! plain words, and audited when they change the fleet (ADR 0012).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ariel_core::chat::console::{ConsoleProvider, Recorded};
use ariel_core::chat::{
    Args, ChannelRef, ChatProvider, Destination, Inbound, Message, Role, Severity, UserRef,
    Visibility,
};
use ariel_core::commands::{self, Context};
use ariel_core::prospero::ProsperoClient;
use ariel_core::records::gonzalo::{
    AuditEntry, AuditResult, Authenticator, BindingOrigin, ChannelConfig, FleetActor, FleetRole,
    Follows, FsStore, GrantScope, Identity, IdentityBinding, NotifyPreset, RoleGrant,
};
use ariel_core::records::{Records, Write};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde_json::json;
use tempfile::TempDir;

const NOW: i64 = 1_760_000_000_000;
const PERSON: &str = "p-ada";

/// How the stub prosperod behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prosperod {
    /// Knows `caliban` (healthy) and `gonzalo` (unreachable).
    Normal,
    /// Refuses Ariel's token on every route.
    RefusesToken,
    /// Fails every request with a 500.
    Broken,
}

#[derive(Clone)]
struct Stub {
    mode: Prosperod,
    spawns: Arc<AtomicUsize>,
    /// Calls to kill or respawn, counted apart from spawns so a test can prove
    /// a refused command never reached prosperod.
    actions: Arc<AtomicUsize>,
}

async fn fleet(State(stub): State<Stub>) -> Response {
    if let Some(failure) = failure(stub.mode) {
        return failure;
    }
    axum::Json(json!({
        "host": "stub",
        "workspaces": [
            {"name": "gonzalo", "health": {"state": "unreachable", "reason": "ssh timed out"}, "agents": []},
            {"name": "caliban", "health": {"state": "healthy"}, "agents": [
                agent("a1", "running"), agent("a2", "running"), agent("a3", "idle"), agent("a4", "done"),
            ]},
            {"name": "prospero", "health": {"state": "healthy"}, "agents": []},
        ]
    }))
    .into_response()
}

fn agent(id: &str, status: &str) -> serde_json::Value {
    json!({"id": id, "name": id, "workspace": "caliban", "status": status,
           "started_at": "2026-09-18T00:00:00Z", "isolated": false, "interactive": false})
}

async fn spawn(State(stub): State<Stub>, Path(workspace): Path<String>) -> Response {
    stub.spawns.fetch_add(1, Ordering::SeqCst);
    if let Some(failure) = failure(stub.mode) {
        return failure;
    }
    if workspace != "caliban" && workspace != "prospero" {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(
                json!({"error": format!("workspace not found: {workspace}"), "kind": "not_found"}),
            ),
        )
            .into_response();
    }
    (
        StatusCode::CREATED,
        axum::Json(json!({"agent_id": "a-new", "workspace": workspace, "isolated": false, "created": true})),
    )
        .into_response()
}

fn failure(mode: Prosperod) -> Option<Response> {
    match mode {
        Prosperod::Normal => None,
        Prosperod::RefusesToken => Some(
            (
                StatusCode::UNAUTHORIZED,
                axum::Json(json!({"error": "unauthorized", "kind": "unauthorized"})),
            )
                .into_response(),
        ),
        Prosperod::Broken => Some(
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(json!({"error": "supervisor crashed", "kind": "internal"})),
            )
                .into_response(),
        ),
    }
}

/// `POST /api/agents/{id}/kill`, which prosperod accepts without a body.
async fn kill(State(stub): State<Stub>, Path(agent): Path<String>) -> Response {
    stub.actions.fetch_add(1, Ordering::SeqCst);
    if let Some(failure) = failure(stub.mode) {
        return failure;
    }
    if agent == "gone" {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error": format!("agent not found: {agent}"), "kind": "not_found"})),
        )
            .into_response();
    }
    StatusCode::ACCEPTED.into_response()
}

/// `POST /api/agents/{id}/respawn`: the restarted agent has a new id.
async fn respawn(State(stub): State<Stub>, Path(_agent): Path<String>) -> Response {
    stub.actions.fetch_add(1, Ordering::SeqCst);
    if let Some(failure) = failure(stub.mode) {
        return failure;
    }
    axum::Json(json!({"agent_id": "a-restarted"})).into_response()
}

/// The call counters a test can read: spawns, and kills plus respawns.
#[derive(Clone, Default)]
struct Calls {
    spawns: Arc<AtomicUsize>,
    actions: Arc<AtomicUsize>,
}

async fn prosperod(mode: Prosperod) -> (ProsperoClient, Calls) {
    let calls = Calls::default();
    let app = Router::new()
        .route("/api/fleet", get(fleet))
        .route("/api/workspaces/{workspace}/agents", post(spawn))
        .route("/api/agents/{agent}/kill", post(kill))
        .route("/api/agents/{agent}/respawn", post(respawn))
        .with_state(Stub {
            mode,
            spawns: Arc::clone(&calls.spawns),
            actions: Arc::clone(&calls.actions),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        ProsperoClient::new(&format!("http://{addr}")).unwrap(),
        calls,
    )
}

/// A prosperod nobody is listening for.
async fn unreachable_prosperod() -> ProsperoClient {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    ProsperoClient::new(&format!("http://{addr}")).unwrap()
}

/// Records, a console, and a prosperod: everything a command touches.
struct Harness {
    _dir: TempDir,
    records: Records,
    console: ConsoleProvider,
    inbound: BoxStream<'static, Inbound>,
    context: Context,
    calls: Calls,
}

impl Harness {
    async fn new(mode: Prosperod) -> Self {
        let (prospero, calls) = prosperod(mode).await;
        Self::with_prospero(prospero, calls)
    }

    fn with_prospero(prospero: ProsperoClient, calls: Calls) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let records = Records::new(
            Arc::new(FsStore::new(dir.path())),
            Identity::new("ariel-test"),
        );
        let console = ConsoleProvider::new();
        let inbound = console.inbound();
        let context = Context {
            records: records.clone(),
            prospero,
            dashboard: Some("https://prospero.example/".parse().unwrap()),
        };
        Self {
            _dir: dir,
            records,
            console,
            inbound,
            context,
            calls,
        }
    }

    /// Link the test user to [`PERSON`].
    async fn link(&self) {
        let binding = IdentityBinding {
            authenticator: Authenticator::Other("console".into()),
            subject: "user-42".into(),
            person: PERSON.into(),
            handle: Some("ada".into()),
            email: None,
            bound_at: NOW,
            bound_by: BindingOrigin::Operator(FleetActor::Service("ariel-test".into())),
        };
        committed(&self.records, binding.key().unwrap(), binding).await;
    }

    async fn grant(&self, scope: GrantScope, role: Role) {
        let grant = RoleGrant {
            person: PERSON.into(),
            scope,
            role: fleet_role(role),
            granted_by: FleetActor::Service("ariel-test".into()),
            granted_at: NOW,
        };
        committed(&self.records, grant.key().unwrap(), grant).await;
    }

    async fn configure(&self, follows: Follows, ceiling: Role) {
        let config = ChannelConfig {
            provider: "console".into(),
            tenant: "t1".into(),
            channel: "ops".into(),
            follows,
            notify: NotifyPreset::All,
            ceiling: fleet_role(ceiling),
        };
        committed(&self.records, config.key().unwrap(), config).await;
    }

    /// A linked user holding `role` fleet-wide, in a fleet channel with
    /// `ceiling`.
    async fn fleet_channel(&self, role: Role, ceiling: Role) {
        self.configure(Follows::Fleet, ceiling).await;
        self.link().await;
        self.grant(GrantScope::Fleet, role).await;
    }

    /// Run `/ariel <name>` and return the one reply it got.
    async fn run(&mut self, name: &str, args: &[(&str, &str)]) -> (Visibility, Message) {
        let before = self.console.log().len();
        self.console.inject_command(
            UserRef::new("console", "t1", "user-42"),
            Destination::Channel(ChannelRef::new("console", "t1", "ops")),
            name,
            Args::from_pairs(args.iter().copied()),
        );
        let Some(Inbound::Command(command)) = self.inbound.next().await else {
            panic!("the console delivered no command");
        };
        commands::respond(&self.context, &command).await;

        let replies: Vec<_> = self.console.log()[before..]
            .iter()
            .filter_map(|entry| match entry {
                Recorded::Replied {
                    visibility,
                    message,
                    ..
                } => Some((*visibility, message.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(replies.len(), 1, "exactly one reply: {replies:?}");
        replies.into_iter().next().unwrap()
    }

    async fn spawn_in(&mut self, workspace: &str) -> (Visibility, Message) {
        self.run(
            "spawn",
            &[("workspace", workspace), ("prompt", "fix the flaky test")],
        )
        .await
    }

    fn spawns(&self) -> usize {
        self.calls.spawns.load(Ordering::SeqCst)
    }

    /// Kills and respawns that reached prosperod.
    fn actions(&self) -> usize {
        self.calls.actions.load(Ordering::SeqCst)
    }

    async fn on_agent(&mut self, command: &str, agent: &str) -> (Visibility, Message) {
        self.run(command, &[("agent", agent)]).await
    }

    async fn audit(&self) -> Vec<AuditEntry> {
        let mut entries = Vec::new();
        for key in self.records.list::<AuditEntry>().await.unwrap() {
            entries.push(
                self.records
                    .get::<AuditEntry>(&key)
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
            );
        }
        entries
    }
}

async fn committed<T>(records: &Records, key: ariel_core::records::gonzalo::RecordKey, value: T)
where
    T: ariel_core::records::FleetRecord + std::fmt::Debug,
{
    let written = records.create(key, value).await.unwrap();
    assert!(matches!(written, Write::Committed(_)), "{written:?}");
}

fn fleet_role(role: Role) -> FleetRole {
    match role {
        Role::Viewer => FleetRole::Viewer,
        Role::Operator => FleetRole::Operator,
        Role::Admin => FleetRole::Admin,
    }
}

const ROLES: [Role; 3] = [Role::Viewer, Role::Operator, Role::Admin];

#[tokio::test]
async fn status_is_allowed_for_every_linked_role_under_every_ceiling() {
    for role in ROLES {
        for ceiling in ROLES {
            let mut harness = Harness::new(Prosperod::Normal).await;
            harness.fleet_channel(role, ceiling).await;

            let (visibility, reply) = harness.run("status", &[]).await;

            assert_eq!(visibility, Visibility::Public, "{role:?} under {ceiling:?}");
            assert_eq!(reply.title.as_deref(), Some("Fleet status"), "{reply:?}");
            assert!(harness.audit().await.is_empty(), "status is not audited");
        }
    }
}

#[tokio::test]
async fn spawn_runs_only_when_both_role_and_ceiling_reach_operator() {
    for role in ROLES {
        for ceiling in ROLES {
            let mut harness = Harness::new(Prosperod::Normal).await;
            harness.fleet_channel(role, ceiling).await;

            let (visibility, reply) = harness.spawn_in("caliban").await;

            let allowed = role.min(ceiling) >= Role::Operator;
            let case = format!("{role:?} under {ceiling:?}: {reply:?}");
            if allowed {
                assert_eq!(visibility, Visibility::Public, "{case}");
                assert_eq!(reply.severity, Severity::Success, "{case}");
                assert!(reply.body.contains("`a-new`"), "{case}");
                assert!(reply.body.contains("`caliban`"), "{case}");
                assert_eq!(harness.spawns(), 1, "{case}");
            } else {
                assert_eq!(visibility, Visibility::Private, "{case}");
                assert!(reply.body.contains("needs **operator**"), "{case}");
                assert!(reply.body.contains("you act as **viewer**"), "{case}");
                assert_eq!(
                    harness.spawns(),
                    0,
                    "a denied spawn never reaches prosperod"
                );
            }

            let audit = harness.audit().await;
            assert_eq!(audit.len(), 1, "one audit entry either way: {case}");
            let entry = &audit[0];
            assert_eq!(entry.action, "command.spawn");
            assert_eq!(entry.target, "caliban");
            assert_eq!(entry.actor, FleetActor::Person(PERSON.into()));
            assert_eq!(
                entry.result,
                if allowed {
                    AuditResult::Succeeded
                } else {
                    AuditResult::Denied
                },
                "{case}"
            );
        }
    }
}

#[tokio::test]
async fn an_unlinked_account_is_told_how_to_link_for_either_command() {
    for command in ["status", "spawn"] {
        let mut harness = Harness::new(Prosperod::Normal).await;
        harness.configure(Follows::Fleet, Role::Admin).await;

        let (visibility, reply) = if command == "spawn" {
            harness.spawn_in("caliban").await
        } else {
            harness.run(command, &[]).await
        };

        assert_eq!(visibility, Visibility::Private);
        assert!(reply.body.contains("/ariel link <token>"), "{reply:?}");
        assert_eq!(harness.spawns(), 0);
    }
}

#[tokio::test]
async fn an_unlinked_spawn_is_audited_against_the_account() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness.configure(Follows::Fleet, Role::Admin).await;

    harness.spawn_in("caliban").await;

    let audit = harness.audit().await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].result, AuditResult::Denied);
    assert_eq!(
        audit[0].actor,
        FleetActor::Unlinked {
            authenticator: Authenticator::Other("console".into()),
            subject: "user-42".into(),
        }
    );
}

#[tokio::test]
async fn an_unconfigured_channel_allows_neither_command_and_says_how_to_fix_it() {
    for command in ["status", "spawn"] {
        let mut harness = Harness::new(Prosperod::Normal).await;
        harness.link().await;
        harness.grant(GrantScope::Fleet, Role::Admin).await;

        let (visibility, reply) = if command == "spawn" {
            harness.spawn_in("caliban").await
        } else {
            harness.run(command, &[]).await
        };

        assert_eq!(visibility, Visibility::Private);
        assert!(reply.body.contains("ariel channel set"), "{reply:?}");
        assert_eq!(harness.spawns(), 0);
    }
}

#[tokio::test]
async fn a_workspace_grant_allows_spawning_there_but_not_a_fleet_wide_status() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness.configure(Follows::Fleet, Role::Admin).await;
    harness.link().await;
    harness
        .grant(GrantScope::Workspace("caliban".into()), Role::Operator)
        .await;

    let (visibility, _) = harness.spawn_in("caliban").await;
    assert_eq!(visibility, Visibility::Public);
    assert_eq!(harness.spawns(), 1);

    let (visibility, reply) = harness.spawn_in("prospero").await;
    assert_eq!(visibility, Visibility::Private);
    assert!(reply.body.contains("no role"), "{reply:?}");
    assert_eq!(
        harness.spawns(),
        1,
        "the other workspace was not spawned in"
    );

    let (visibility, reply) = harness.run("status", &[]).await;
    assert_eq!(visibility, Visibility::Private);
    assert!(reply.body.contains("fleet-wide role"), "{reply:?}");
}

#[tokio::test]
async fn a_channel_acts_only_on_the_workspaces_it_follows() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness
        .configure(
            Follows::Workspaces {
                names: ["caliban".to_owned()].into_iter().collect(),
            },
            Role::Admin,
        )
        .await;
    harness.link().await;
    harness.grant(GrantScope::Fleet, Role::Admin).await;

    let (visibility, reply) = harness.spawn_in("prospero").await;
    assert_eq!(visibility, Visibility::Private);
    assert!(
        reply.body.contains("doesn't follow workspace `prospero`"),
        "{reply:?}"
    );
    assert_eq!(harness.spawns(), 0);

    // Status shows only what the channel follows.
    let (_, reply) = harness.run("status", &[]).await;
    assert!(reply.body.contains("**caliban**"), "{reply:?}");
    assert!(!reply.body.contains("prospero"), "{reply:?}");
    assert!(!reply.body.contains("gonzalo"), "{reply:?}");
}

#[tokio::test]
async fn status_summarizes_each_workspace_and_links_the_dashboard() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness.fleet_channel(Role::Viewer, Role::Viewer).await;

    let (_, reply) = harness.run("status", &[]).await;

    let lines: Vec<_> = reply.body.lines().collect();
    assert_eq!(
        lines,
        [
            "3 active agents across 3 workspaces.",
            "- **caliban**: 2 running, 1 idle, 1 done",
            "- **gonzalo**: unreachable: ssh timed out",
            "- **prospero**: no agents",
        ],
        "{reply:?}"
    );
    assert_eq!(
        reply.severity,
        Severity::Warning,
        "a workspace is unreachable"
    );
    assert_eq!(
        reply.link.as_ref().map(ToString::to_string).as_deref(),
        Some("https://prospero.example/")
    );
}

#[tokio::test]
async fn spawning_in_an_unknown_workspace_says_so_and_is_audited_as_failed() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness.fleet_channel(Role::Operator, Role::Operator).await;

    let (visibility, reply) = harness.spawn_in("ghost").await;

    assert_eq!(visibility, Visibility::Private);
    assert_eq!(reply.severity, Severity::Failure);
    assert_eq!(
        reply.body,
        "prosperod doesn't know a workspace called `ghost`."
    );
    let audit = harness.audit().await;
    assert_eq!(audit.len(), 1);
    assert!(
        matches!(&audit[0].result, AuditResult::Failed(why) if why.contains("workspace not found: ghost")),
        "{:?}",
        audit[0].result
    );
    assert_eq!(audit[0].target, "ghost");
}

#[tokio::test]
async fn prosperod_refusing_ariels_token_is_explained_not_shown_as_a_status_code() {
    for command in ["status", "spawn"] {
        let mut harness = Harness::new(Prosperod::RefusesToken).await;
        harness.fleet_channel(Role::Admin, Role::Admin).await;

        let (visibility, reply) = if command == "spawn" {
            harness.spawn_in("caliban").await
        } else {
            harness.run(command, &[]).await
        };

        assert_eq!(visibility, Visibility::Private);
        assert!(reply.body.contains("Ariel's own credentials"), "{reply:?}");
        assert!(!reply.body.contains("401"), "{reply:?}");
    }
}

#[tokio::test]
async fn a_prosperod_server_error_is_reported_with_its_message() {
    let mut harness = Harness::new(Prosperod::Broken).await;
    harness.fleet_channel(Role::Admin, Role::Admin).await;

    let (visibility, reply) = harness.spawn_in("caliban").await;

    assert_eq!(visibility, Visibility::Private);
    assert_eq!(
        reply.body,
        "prosperod failed to carry it out: supervisor crashed"
    );
    let audit = harness.audit().await;
    assert!(
        matches!(&audit[0].result, AuditResult::Failed(why) if why.contains("supervisor crashed")),
        "{:?}",
        audit[0].result
    );
}

#[tokio::test]
async fn an_unreachable_prosperod_asks_to_try_again() {
    let prospero = unreachable_prosperod().await;
    let mut harness = Harness::with_prospero(prospero, Calls::default());
    harness.fleet_channel(Role::Admin, Role::Admin).await;

    let (visibility, reply) = harness.run("status", &[]).await;

    assert_eq!(visibility, Visibility::Private);
    assert_eq!(
        reply.body,
        "Ariel couldn't reach prosperod; try again shortly."
    );
}

#[tokio::test]
async fn spawn_without_a_workspace_or_prompt_shows_usage_and_does_nothing() {
    for args in [
        vec![("workspace", "caliban")],
        vec![("prompt", "fix it")],
        vec![("workspace", "caliban"), ("prompt", "   ")],
    ] {
        let mut harness = Harness::new(Prosperod::Normal).await;
        harness.fleet_channel(Role::Admin, Role::Admin).await;

        let (visibility, reply) = harness.run("spawn", &args).await;

        assert_eq!(visibility, Visibility::Private);
        assert!(
            reply.body.contains("/ariel spawn <workspace> <prompt>"),
            "{reply:?}"
        );
        assert_eq!(harness.spawns(), 0);
        assert!(harness.audit().await.is_empty(), "{args:?}");
    }
}

#[tokio::test]
async fn an_unknown_command_gets_a_private_answer() {
    let mut harness = Harness::new(Prosperod::Normal).await;

    let (visibility, reply) = harness.run("dance", &[]).await;

    assert_eq!(visibility, Visibility::Private);
    assert!(reply.body.contains("`/ariel dance`"), "{reply:?}");
}

#[test]
fn every_command_is_registered_once() {
    let names: Vec<_> = commands::ALL.iter().map(|spec| spec.name).collect();
    assert_eq!(names, ["link", "status", "spawn", "kill", "respawn"]);
    assert_eq!(commands::STATUS.min_role, Role::Viewer);
    assert_eq!(commands::SPAWN.min_role, Role::Operator);
    assert_eq!(commands::KILL.min_role, Role::Operator);
    assert_eq!(commands::RESPAWN.min_role, Role::Operator);
}

// --- /ariel kill and /ariel respawn (#57) ---------------------------------

#[tokio::test]
async fn killing_and_respawning_need_both_role_and_ceiling_at_operator() {
    for command in ["kill", "respawn"] {
        for role in ROLES {
            for ceiling in ROLES {
                let mut harness = Harness::new(Prosperod::Normal).await;
                harness.fleet_channel(role, ceiling).await;

                let (visibility, reply) = harness.on_agent(command, "a1").await;

                let allowed = role.min(ceiling) >= Role::Operator;
                let case = format!("{command} as {role:?} under {ceiling:?}: {reply:?}");
                if allowed {
                    assert_eq!(visibility, Visibility::Public, "{case}");
                    assert_eq!(reply.severity, Severity::Success, "{case}");
                    assert!(reply.body.contains("`a1`"), "{case}");
                    assert!(reply.body.contains("`caliban`"), "{case}");
                    assert_eq!(harness.actions(), 1, "{case}");
                } else {
                    assert_eq!(visibility, Visibility::Private, "{case}");
                    assert!(reply.body.contains("needs **operator**"), "{case}");
                    assert_eq!(
                        harness.actions(),
                        0,
                        "a refused {command} reached prosperod"
                    );
                }

                let audit = harness.audit().await;
                assert_eq!(audit.len(), 1, "one audit entry either way: {case}");
                assert_eq!(audit[0].action, format!("command.{command}"));
                assert_eq!(audit[0].target, "caliban", "audited against the workspace");
                assert_eq!(
                    audit[0].result,
                    if allowed {
                        AuditResult::Succeeded
                    } else {
                        AuditResult::Denied
                    },
                    "{case}"
                );
            }
        }
    }
}

#[tokio::test]
async fn respawning_names_the_new_agent() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness.fleet_channel(Role::Operator, Role::Operator).await;

    let (_, reply) = harness.on_agent("respawn", "a1").await;

    assert!(reply.body.contains("`a-restarted`"), "{reply:?}");
    assert!(
        reply.body.contains("`a1`"),
        "the old id is named too: {reply:?}"
    );
}

#[tokio::test]
async fn an_agent_the_fleet_does_not_have_is_refused_without_calling_prosperod() {
    for command in ["kill", "respawn"] {
        let mut harness = Harness::new(Prosperod::Normal).await;
        harness.fleet_channel(Role::Admin, Role::Admin).await;

        let (visibility, reply) = harness.on_agent(command, "nobody").await;

        assert_eq!(visibility, Visibility::Private);
        assert_eq!(reply.body, "No agent called `nobody` is in the fleet.");
        assert_eq!(harness.actions(), 0, "prosperod was asked anyway");
        assert!(
            harness.audit().await.is_empty(),
            "nothing was attempted, so nothing is audited"
        );
    }
}

#[tokio::test]
async fn an_unconfigured_channel_learns_nothing_about_the_fleet() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness.link().await;
    harness.grant(GrantScope::Fleet, Role::Admin).await;

    // Asking about an agent that does not exist is the case that tells the two
    // orderings apart: if the channel were checked after the fleet lookup, the
    // reply would say whether `nobody` is in the fleet, which is precisely what
    // an unconfigured channel must not be able to find out.
    let (visibility, reply) = harness.on_agent("kill", "nobody").await;

    assert_eq!(visibility, Visibility::Private);
    assert!(reply.body.contains("ariel channel set"), "{reply:?}");
    assert!(
        !reply.body.contains("in the fleet"),
        "an unconfigured channel was told about the fleet: {reply:?}"
    );
    assert_eq!(harness.actions(), 0);

    // And the same refusal for an agent that does exist, so the two are
    // indistinguishable from outside.
    let (_, existing) = harness.on_agent("kill", "a1").await;
    assert_eq!(existing.body, reply.body);
}

#[tokio::test]
async fn an_agent_outside_the_channels_workspaces_is_refused() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness
        .configure(
            Follows::Workspaces {
                names: ["prospero".to_owned()].into_iter().collect(),
            },
            Role::Admin,
        )
        .await;
    harness.link().await;
    harness.grant(GrantScope::Fleet, Role::Admin).await;

    // `a1` is in caliban, which this channel does not follow.
    let (visibility, reply) = harness.on_agent("kill", "a1").await;

    assert_eq!(visibility, Visibility::Private);
    assert!(
        reply.body.contains("doesn't follow workspace `caliban`"),
        "{reply:?}"
    );
    assert_eq!(harness.actions(), 0);
}

#[tokio::test]
async fn killing_without_naming_an_agent_shows_usage() {
    let mut harness = Harness::new(Prosperod::Normal).await;
    harness.fleet_channel(Role::Admin, Role::Admin).await;

    let (visibility, reply) = harness.run("kill", &[("agent", "  ")]).await;

    assert_eq!(visibility, Visibility::Private);
    assert!(reply.body.contains("/ariel kill <agent>"), "{reply:?}");
    assert_eq!(harness.actions(), 0);
    assert!(harness.audit().await.is_empty());
}

#[tokio::test]
async fn a_prosperod_failure_on_kill_is_readable_and_audited() {
    let mut harness = Harness::new(Prosperod::Broken).await;
    harness.fleet_channel(Role::Admin, Role::Admin).await;

    // The fleet read fails first on a broken prosperod, so the person is told
    // that rather than left with a status code.
    let (visibility, reply) = harness.on_agent("kill", "a1").await;

    assert_eq!(visibility, Visibility::Private);
    assert_eq!(reply.severity, Severity::Failure);
    assert!(
        reply.body.contains("supervisor crashed"),
        "prosperod's own words reach the person: {reply:?}"
    );
}
