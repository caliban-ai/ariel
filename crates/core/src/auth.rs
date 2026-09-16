//! Two-key command authorization and its audit trail (#17, ADR 0009).
//!
//! A command is authorized on two keys: the **person's role** and the
//! **channel's ceiling**. It runs at the lower of the two. Resolution goes chat
//! account → identity binding → person → role grants, all gonzalo records
//! ([ADR 0003](../../docs/adr/0003-no-state-of-its-own.md)).
//!
//! The rules, in the order they are checked:
//!
//! 1. **The channel must be configured.** An unconfigured channel allows no
//!    commands (account linking is the exception, and is not a command this
//!    module authorizes).
//! 2. **A command may act only on a workspace its channel follows.** A channel
//!    following the whole fleet can act on any workspace.
//! 3. **An unlinked account gets nothing.**
//! 4. **The person's role is the highest applicable grant.** A fleet-wide grant
//!    always applies; a grant scoped to one workspace applies only to commands
//!    acting on that workspace, never to other workspaces or to commands that act
//!    on no workspace.
//! 5. **The effective role is the lower of that role and the channel's ceiling**,
//!    and must reach the command's `min_role`.
//!
//! **Audit.** A command is *mutating* when it needs more than `viewer`. Every
//! mutating command leaves exactly one audit entry: a denial is recorded by
//! [`authorize`] itself, so no caller can forget it, and an allowed command is
//! recorded by [`record_outcome`] once it has run, with how it went. Read-only
//! commands are not audited.

use crate::chat::{ChannelRef, CommandSpec, ProviderId, Role, UserRef};
use crate::records::gonzalo::{
    AuditEntry, AuditResult, Authenticator, ChannelConfig, FleetActor, FleetKeyError, FleetRole,
    Follows, GrantScope, IdentityBinding, RoleGrant,
};
use crate::records::{Records, RecordsError, now_ms};

/// One command someone asked to run.
#[derive(Debug, Clone)]
pub struct Request<'a> {
    pub user: &'a UserRef,
    /// Where it was run.
    pub channel: &'a ChannelRef,
    pub command: &'a CommandSpec,
    /// The workspace the command acts on, or `None` for a fleet-wide command.
    pub workspace: Option<&'a str>,
    /// The chat message the command came from, for the audit trail.
    pub surface_ref: Option<String>,
}

/// Whether a command may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed {
        /// The person the account is linked to.
        person: String,
        /// The role the command runs with: the lower of the two keys.
        effective: Role,
    },
    Denied(Denial),
}

/// Why a command may not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Denial {
    /// The channel has no configuration, so it allows no commands.
    ChannelNotConfigured,
    /// The command acts on a workspace this channel does not follow.
    WorkspaceNotFollowed { workspace: String },
    /// The chat account is not linked to a person.
    Unlinked,
    /// The person holds no grant that applies to this command.
    NoRole,
    /// The effective role is below what the command needs.
    InsufficientRole { effective: Role, required: Role },
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)]
    Records(#[from] RecordsError),
    #[error("invalid record key: {0}")]
    Key(#[from] FleetKeyError),
}

/// The authenticator that vouches for accounts on this chat provider.
pub fn authenticator(provider: &ProviderId) -> Authenticator {
    match provider.as_str() {
        "discord" => Authenticator::Discord,
        "slack" => Authenticator::Slack,
        "teams" => Authenticator::Teams,
        other => Authenticator::Other(other.to_owned()),
    }
}

/// Decide whether `request` may run. A denied mutating command is audited here.
pub async fn authorize(records: &Records, request: &Request<'_>) -> Result<Decision, AuthError> {
    let person = linked_person(records, request.user).await?;
    let decision = decide(records, request, person.as_deref()).await?;

    if let Decision::Denied(_) = &decision
        && mutating(request.command)
    {
        audit(
            records,
            request,
            actor(request, person.as_deref()),
            AuditResult::Denied,
        )
        .await?;
    }
    Ok(decision)
}

/// Record how an allowed mutating command went. Does nothing for a read-only
/// command, or for a denial, which [`authorize`] has already recorded.
pub async fn record_outcome(
    records: &Records,
    request: &Request<'_>,
    decision: &Decision,
    result: AuditResult,
) -> Result<(), AuthError> {
    let Decision::Allowed { person, .. } = decision else {
        return Ok(());
    };
    if !mutating(request.command) {
        return Ok(());
    }
    audit(records, request, FleetActor::Person(person.clone()), result).await
}

/// A command that needs more than `viewer` changes the fleet.
fn mutating(command: &CommandSpec) -> bool {
    command.min_role > Role::Viewer
}

async fn decide(
    records: &Records,
    request: &Request<'_>,
    person: Option<&str>,
) -> Result<Decision, AuthError> {
    let channel = request.channel;
    let config_key = ChannelConfig::key_for(
        channel.provider.as_str(),
        channel.tenant.as_str(),
        &channel.channel,
    )?;
    let Some(config) = records.get::<ChannelConfig>(&config_key).await? else {
        return Ok(Decision::Denied(Denial::ChannelNotConfigured));
    };

    if let Some(workspace) = request.workspace
        && !follows(&config.value.follows, workspace)
    {
        return Ok(Decision::Denied(Denial::WorkspaceNotFollowed {
            workspace: workspace.to_owned(),
        }));
    }

    let Some(person) = person else {
        return Ok(Decision::Denied(Denial::Unlinked));
    };

    let Some(role) = person_role(records, person, request.workspace).await? else {
        return Ok(Decision::Denied(Denial::NoRole));
    };

    let effective = role.min(role_of(config.value.ceiling));
    let required = request.command.min_role;
    if effective < required {
        return Ok(Decision::Denied(Denial::InsufficientRole {
            effective,
            required,
        }));
    }
    Ok(Decision::Allowed {
        person: person.to_owned(),
        effective,
    })
}

fn follows(follows: &Follows, workspace: &str) -> bool {
    match follows {
        Follows::Fleet => true,
        Follows::Workspaces { names } => names.contains(workspace),
    }
}

/// The person this chat account is bound to, if any.
async fn linked_person(records: &Records, user: &UserRef) -> Result<Option<String>, AuthError> {
    let key = IdentityBinding::key_for(&authenticator(&user.provider), &user.user)?;
    Ok(records
        .get::<IdentityBinding>(&key)
        .await?
        .map(|binding| binding.value.person))
}

/// The highest role `person` holds for a command on `workspace`.
async fn person_role(
    records: &Records,
    person: &str,
    workspace: Option<&str>,
) -> Result<Option<Role>, AuthError> {
    let mut scopes = vec![GrantScope::Fleet];
    if let Some(workspace) = workspace {
        scopes.push(GrantScope::Workspace(workspace.to_owned()));
    }

    let mut highest: Option<Role> = None;
    for scope in scopes {
        let key = RoleGrant::key_for(person, &scope)?;
        if let Some(grant) = records.get::<RoleGrant>(&key).await? {
            let role = role_of(grant.value.role);
            highest = Some(highest.map_or(role, |current| current.max(role)));
        }
    }
    Ok(highest)
}

fn role_of(role: FleetRole) -> Role {
    match role {
        FleetRole::Viewer => Role::Viewer,
        FleetRole::Operator => Role::Operator,
        FleetRole::Admin => Role::Admin,
    }
}

/// Who to record: the person, or the account itself when it is not linked.
fn actor(request: &Request<'_>, person: Option<&str>) -> FleetActor {
    match person {
        Some(person) => FleetActor::Person(person.to_owned()),
        None => FleetActor::Unlinked {
            authenticator: authenticator(&request.user.provider),
            subject: request.user.user.clone(),
        },
    }
}

async fn audit(
    records: &Records,
    request: &Request<'_>,
    actor: FleetActor,
    result: AuditResult,
) -> Result<(), AuthError> {
    records
        .append_audit(&AuditEntry {
            actor,
            action: format!("command.{}", request.command.name),
            target: request.workspace.unwrap_or("fleet").to_owned(),
            at: now_ms(),
            surface_ref: request.surface_ref.clone(),
            result,
        })
        .await?;
    Ok(())
}
