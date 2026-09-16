//! Two-key command authorization and its audit trail (#17, ADR 0009).
//!
//! A command runs at the lower of the person's role and the channel's ceiling.
//! An unlinked user gets nothing, and a grant scoped to one workspace counts only
//! in that workspace.

use std::sync::Arc;

use ariel_core::auth::{self, Decision, Denial, Request};
use ariel_core::chat::{ChannelRef, CommandSpec, Role, UserRef};
use ariel_core::records::gonzalo::{
    AuditEntry, AuditResult, Authenticator, BindingOrigin, ChannelConfig, FleetActor, FleetRole,
    Follows, FsStore, GrantScope, Identity, IdentityBinding, NotifyPreset, RoleGrant,
};
use ariel_core::records::{Records, Write};
use tempfile::TempDir;

const NOW: i64 = 1_760_000_000_000;

fn records() -> (TempDir, Records) {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(FsStore::new(dir.path()));
    (dir, Records::new(store, Identity::new("ariel-test")))
}

fn user() -> UserRef {
    UserRef::new("discord", "guild-1", "user-42")
}

fn channel() -> ChannelRef {
    ChannelRef::new("discord", "guild-1", "ops")
}

/// A command needing `min_role`.
fn command(name: &'static str, min_role: Role) -> CommandSpec {
    CommandSpec {
        name,
        summary: "test command",
        args: &[],
        min_role,
    }
}

fn fleet_role(role: Role) -> FleetRole {
    match role {
        Role::Viewer => FleetRole::Viewer,
        Role::Operator => FleetRole::Operator,
        Role::Admin => FleetRole::Admin,
    }
}

async fn committed<T>(records: &Records, key: ariel_core::records::gonzalo::RecordKey, value: T)
where
    T: ariel_core::records::FleetRecord + std::fmt::Debug,
{
    let written = records.create(key, value).await.unwrap();
    assert!(matches!(written, Write::Committed(_)), "{written:?}");
}

/// Bind the test user's Discord account to `person`.
async fn link(records: &Records, person: &str) {
    let binding = IdentityBinding {
        authenticator: Authenticator::Discord,
        subject: "user-42".into(),
        person: person.into(),
        handle: Some("ada".into()),
        email: None,
        bound_at: NOW,
        bound_by: BindingOrigin::Operator(FleetActor::Service("ariel-test".into())),
    };
    committed(records, binding.key().unwrap(), binding).await;
}

async fn grant(records: &Records, person: &str, scope: GrantScope, role: Role) {
    let grant = RoleGrant {
        person: person.into(),
        scope,
        role: fleet_role(role),
        granted_by: FleetActor::Service("ariel-test".into()),
        granted_at: NOW,
    };
    committed(records, grant.key().unwrap(), grant).await;
}

async fn configure(records: &Records, follows: Follows, ceiling: Role) {
    let config = ChannelConfig {
        provider: "discord".into(),
        tenant: "guild-1".into(),
        channel: "ops".into(),
        follows,
        notify: NotifyPreset::All,
        ceiling: fleet_role(ceiling),
    };
    committed(records, config.key().unwrap(), config).await;
}

fn request<'a>(
    user: &'a UserRef,
    channel: &'a ChannelRef,
    command: &'a CommandSpec,
    workspace: Option<&'a str>,
) -> Request<'a> {
    Request {
        user,
        channel,
        command,
        workspace,
        surface_ref: Some("discord:guild-1:ops:msg-1".into()),
    }
}

async fn audit_entries(records: &Records) -> Vec<AuditEntry> {
    let mut entries = Vec::new();
    for key in records.list::<AuditEntry>().await.unwrap() {
        entries.push(
            records
                .get::<AuditEntry>(&key)
                .await
                .unwrap()
                .unwrap()
                .value,
        );
    }
    entries
}

const ROLES: [Role; 3] = [Role::Viewer, Role::Operator, Role::Admin];

#[tokio::test]
async fn every_role_ceiling_and_command_combination_follows_the_two_key_rule() {
    // `None` is a linked person with no grant at all.
    let person_roles = [
        None,
        Some(Role::Viewer),
        Some(Role::Operator),
        Some(Role::Admin),
    ];

    let mut checked = 0;
    for person_role in person_roles {
        for ceiling in ROLES {
            for required in ROLES {
                let (_dir, records) = records();
                link(&records, "p1").await;
                if let Some(role) = person_role {
                    grant(&records, "p1", GrantScope::Fleet, role).await;
                }
                configure(&records, Follows::Fleet, ceiling).await;

                let spec = command("act", required);
                let (u, c) = (user(), channel());
                let decision = auth::authorize(&records, &request(&u, &c, &spec, Some("caliban")))
                    .await
                    .unwrap();

                let effective = person_role.map(|role| role.min(ceiling));
                let expected = effective.is_some_and(|effective| effective >= required);
                assert_eq!(
                    matches!(decision, Decision::Allowed { .. }),
                    expected,
                    "person {person_role:?}, ceiling {ceiling:?}, command needs {required:?}: \
                     got {decision:?}"
                );
                if let Decision::Allowed { effective: got, .. } = decision {
                    assert_eq!(Some(got), effective, "effective role is the lower key");
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 4 * 3 * 3, "the table is exhaustive");
}

#[tokio::test]
async fn an_unlinked_user_gets_nothing_even_in_a_permissive_channel() {
    let (_dir, records) = records();
    configure(&records, Follows::Fleet, Role::Admin).await;

    let spec = command("status", Role::Viewer);
    let (u, c) = (user(), channel());
    let decision = auth::authorize(&records, &request(&u, &c, &spec, None))
        .await
        .unwrap();
    assert!(
        matches!(decision, Decision::Denied(Denial::Unlinked)),
        "{decision:?}"
    );
}

#[tokio::test]
async fn a_workspace_grant_does_not_reach_other_workspaces() {
    let (_dir, records) = records();
    link(&records, "p1").await;
    grant(
        &records,
        "p1",
        GrantScope::Workspace("caliban".into()),
        Role::Operator,
    )
    .await;
    configure(&records, Follows::Fleet, Role::Admin).await;

    let spawn = command("spawn", Role::Operator);
    let (u, c) = (user(), channel());

    let inside = auth::authorize(&records, &request(&u, &c, &spawn, Some("caliban")))
        .await
        .unwrap();
    assert!(
        matches!(
            inside,
            Decision::Allowed {
                effective: Role::Operator,
                ..
            }
        ),
        "the grant applies in its own workspace: {inside:?}"
    );

    let elsewhere = auth::authorize(&records, &request(&u, &c, &spawn, Some("prospero")))
        .await
        .unwrap();
    assert!(
        matches!(elsewhere, Decision::Denied(Denial::NoRole)),
        "a caliban grant must not authorize prospero: {elsewhere:?}"
    );

    let fleet_wide = auth::authorize(&records, &request(&u, &c, &spawn, None))
        .await
        .unwrap();
    assert!(
        matches!(fleet_wide, Decision::Denied(Denial::NoRole)),
        "nor a command with no workspace: {fleet_wide:?}"
    );
}

#[tokio::test]
async fn a_fleet_grant_and_a_workspace_grant_combine_to_the_higher_role() {
    let (_dir, records) = records();
    link(&records, "p1").await;
    grant(&records, "p1", GrantScope::Fleet, Role::Viewer).await;
    grant(
        &records,
        "p1",
        GrantScope::Workspace("caliban".into()),
        Role::Admin,
    )
    .await;
    configure(&records, Follows::Fleet, Role::Admin).await;

    let spec = command("act", Role::Admin);
    let (u, c) = (user(), channel());
    let decision = auth::authorize(&records, &request(&u, &c, &spec, Some("caliban")))
        .await
        .unwrap();
    assert!(
        matches!(
            decision,
            Decision::Allowed {
                effective: Role::Admin,
                ..
            }
        ),
        "{decision:?}"
    );
}

#[tokio::test]
async fn an_unconfigured_channel_allows_no_commands() {
    let (_dir, records) = records();
    link(&records, "p1").await;
    grant(&records, "p1", GrantScope::Fleet, Role::Admin).await;

    let spec = command("status", Role::Viewer);
    let (u, c) = (user(), channel());
    let decision = auth::authorize(&records, &request(&u, &c, &spec, None))
        .await
        .unwrap();
    assert!(
        matches!(decision, Decision::Denied(Denial::ChannelNotConfigured)),
        "{decision:?}"
    );
}

#[tokio::test]
async fn a_command_may_act_only_on_a_workspace_its_channel_follows() {
    let (_dir, records) = records();
    link(&records, "p1").await;
    grant(&records, "p1", GrantScope::Fleet, Role::Admin).await;
    configure(
        &records,
        Follows::workspaces(["caliban"]).unwrap(),
        Role::Admin,
    )
    .await;

    let spawn = command("spawn", Role::Operator);
    let (u, c) = (user(), channel());
    let decision = auth::authorize(&records, &request(&u, &c, &spawn, Some("prospero")))
        .await
        .unwrap();
    assert!(
        matches!(
            decision,
            Decision::Denied(Denial::WorkspaceNotFollowed { ref workspace }) if workspace == "prospero"
        ),
        "{decision:?}"
    );
}

#[tokio::test]
async fn a_denied_mutating_command_is_audited() {
    let (_dir, records) = records();
    link(&records, "p1").await;
    grant(&records, "p1", GrantScope::Fleet, Role::Viewer).await;
    configure(&records, Follows::Fleet, Role::Admin).await;

    let spawn = command("spawn", Role::Operator);
    let (u, c) = (user(), channel());
    let decision = auth::authorize(&records, &request(&u, &c, &spawn, Some("caliban")))
        .await
        .unwrap();
    assert!(matches!(decision, Decision::Denied(_)), "{decision:?}");

    let entries = audit_entries(&records).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    assert_eq!(entry.action, "command.spawn");
    assert_eq!(entry.target, "caliban");
    assert_eq!(entry.result, AuditResult::Denied);
    assert_eq!(entry.actor, FleetActor::Person("p1".into()));
    assert_eq!(
        entry.surface_ref.as_deref(),
        Some("discord:guild-1:ops:msg-1")
    );
}

#[tokio::test]
async fn an_unlinked_attempt_at_a_mutating_command_is_audited_against_the_account() {
    let (_dir, records) = records();
    configure(&records, Follows::Fleet, Role::Admin).await;

    let spawn = command("spawn", Role::Operator);
    let (u, c) = (user(), channel());
    auth::authorize(&records, &request(&u, &c, &spawn, Some("caliban")))
        .await
        .unwrap();

    let entries = audit_entries(&records).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(
        entries[0].actor,
        FleetActor::Unlinked {
            authenticator: Authenticator::Discord,
            subject: "user-42".into(),
        }
    );
    assert_eq!(entries[0].result, AuditResult::Denied);
}

#[tokio::test]
async fn a_denied_read_only_command_is_not_audited() {
    let (_dir, records) = records();
    configure(&records, Follows::Fleet, Role::Viewer).await;

    let status = command("status", Role::Viewer);
    let (u, c) = (user(), channel());
    auth::authorize(&records, &request(&u, &c, &status, None))
        .await
        .unwrap();

    assert!(
        audit_entries(&records).await.is_empty(),
        "only mutating commands are audited"
    );
}

#[tokio::test]
async fn an_allowed_mutating_command_is_audited_with_its_outcome() {
    let (_dir, records) = records();
    link(&records, "p1").await;
    grant(&records, "p1", GrantScope::Fleet, Role::Operator).await;
    configure(&records, Follows::Fleet, Role::Operator).await;

    let spawn = command("spawn", Role::Operator);
    let (u, c) = (user(), channel());
    let req = request(&u, &c, &spawn, Some("caliban"));
    let decision = auth::authorize(&records, &req).await.unwrap();
    assert!(matches!(decision, Decision::Allowed { .. }), "{decision:?}");
    assert!(
        audit_entries(&records).await.is_empty(),
        "an allowed command is audited once it has run, not before"
    );

    auth::record_outcome(&records, &req, &decision, AuditResult::Succeeded)
        .await
        .unwrap();
    let entries = audit_entries(&records).await;
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].result, AuditResult::Succeeded);
    assert_eq!(entries[0].actor, FleetActor::Person("p1".into()));
}

#[test]
fn providers_map_to_their_authenticators() {
    use ariel_core::chat::ProviderId;
    assert_eq!(
        auth::authenticator(&ProviderId::new("discord")),
        Authenticator::Discord
    );
    assert_eq!(
        auth::authenticator(&ProviderId::new("slack")),
        Authenticator::Slack
    );
    assert_eq!(
        auth::authenticator(&ProviderId::new("teams")),
        Authenticator::Teams
    );
    assert_eq!(
        auth::authenticator(&ProviderId::new("console")),
        Authenticator::Other("console".into())
    );
}
