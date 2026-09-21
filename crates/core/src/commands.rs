//! The `/ariel` commands and the router that answers them (#20, ADR 0012).
//!
//! - `/ariel link <token>` ties a chat account to a person ([`crate::link`]).
//! - `/ariel status` (viewer) summarizes the part of the fleet the channel
//!   follows. It acts on no one workspace, so it needs a fleet-wide grant.
//! - `/ariel spawn <workspace> <prompt>` (operator) starts an agent.
//! - `/ariel kill <agent>` and `/ariel respawn <agent>` (operator) stop and
//!   restart one. They name an agent rather than a workspace, so the agent's
//!   workspace is resolved from the fleet before it can be authorized.
//!
//! Every command except `link` goes through [`auth::authorize`], which audits a
//! denied mutating command itself. The router records how an allowed one went
//! with [`auth::record_outcome`], so each mutating command leaves exactly one
//! audit entry whichever way it ends.
//!
//! **Replies.** A result is public, so the channel sees the fleet and who
//! started what. A denial or failure is private to the person who asked: it
//! concerns only them, and a busy channel need not read it.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::auth::{self, AuthError, Decision, Denial, Request};
use crate::channels;
use crate::chat::{
    ArgSpec, ChannelRef, Command, CommandSpec, Message, Role, Severity, Url, Visibility,
};
use crate::link;
use crate::prospero::types::{AgentStatus, FleetSnapshot, SpawnRequest, WorkspaceHealth};
use crate::prospero::{ClientError, ProsperoClient};
use crate::records::gonzalo::{
    AuditResult, ChannelConfig, FleetActor, FleetRole, Follows, GrantScope,
};
use crate::records::{Records, now_ms};

/// `/ariel status`: read-only, so open to viewers and not audited.
pub const STATUS: CommandSpec = CommandSpec {
    name: "status",
    summary: "Summarize the fleet this channel follows",
    args: &[],
    min_role: Role::Viewer,
};

const WORKSPACE_ARG: ArgSpec = ArgSpec {
    name: "workspace",
    summary: "The workspace to start the agent in",
    required: true,
};

const PROMPT_ARG: ArgSpec = ArgSpec {
    name: "prompt",
    summary: "What the agent should do",
    required: true,
};

/// `/ariel spawn <workspace> <prompt>`: changes the fleet, so it needs an
/// operator and is audited.
pub const SPAWN: CommandSpec = CommandSpec {
    name: "spawn",
    summary: "Start an agent in a workspace",
    args: &[WORKSPACE_ARG, PROMPT_ARG],
    min_role: Role::Operator,
};

const AGENT_ARG: ArgSpec = ArgSpec {
    name: "agent",
    summary: "The agent's id, as the fleet status and notifications show it",
    required: true,
};

/// `/ariel kill <agent>`: stops an agent, so it needs an operator in the
/// agent's own workspace, and is audited.
pub const KILL: CommandSpec = CommandSpec {
    name: "kill",
    summary: "Stop an agent",
    args: &[AGENT_ARG],
    min_role: Role::Operator,
};

/// `/ariel respawn <agent>`: restarts an agent from its original prompt. The
/// restarted agent has a new id.
pub const RESPAWN: CommandSpec = CommandSpec {
    name: "respawn",
    summary: "Restart an agent, from the prompt it was given",
    args: &[AGENT_ARG],
    min_role: Role::Operator,
};

/// `/ariel channel`: what this channel follows, hears and allows. Read-only,
/// so a viewer may ask.
pub const CHANNEL: CommandSpec = CommandSpec {
    name: "channel",
    summary: "Show what this channel follows, hears and allows",
    args: &[],
    min_role: Role::Viewer,
};

const FOLLOWS_ARG: ArgSpec = ArgSpec {
    name: "follows",
    summary: "`fleet`, or a comma-separated list of workspace names",
    required: false,
};

const NOTIFY_ARG: ArgSpec = ArgSpec {
    name: "notify",
    summary: "How much this channel hears: all, terminal or failures",
    required: false,
};

const CEILING_ARG: ArgSpec = ArgSpec {
    name: "ceiling",
    summary: "The highest role any command here runs with: viewer, operator or admin",
    required: false,
};

/// `/ariel configure`: change this channel's configuration. Admin only
/// (ADR 0009), and audited.
pub const CONFIGURE: CommandSpec = CommandSpec {
    name: "configure",
    summary: "Change what this channel follows, hears or allows",
    args: &[FOLLOWS_ARG, NOTIFY_ARG, CEILING_ARG],
    min_role: Role::Admin,
};

const ROLE_ARG: ArgSpec = ArgSpec {
    name: "role",
    summary: "The role the person receives: viewer, operator or admin",
    required: true,
};

const WORKSPACE_ONLY_ARG: ArgSpec = ArgSpec {
    name: "workspace",
    summary: "Grant the role in this workspace only, instead of fleet-wide",
    required: false,
};

const HOURS_ARG: ArgSpec = ArgSpec {
    name: "hours",
    summary: "How many hours the token stays redeemable (24 by default)",
    required: false,
};

/// `/ariel invite`: mint a one-time link token. Admin only, and the token is
/// handed back privately or not at all.
pub const INVITE: CommandSpec = CommandSpec {
    name: "invite",
    summary: "Mint a one-time link token for someone to redeem with /ariel link",
    args: &[ROLE_ARG, WORKSPACE_ONLY_ARG, HOURS_ARG],
    min_role: Role::Admin,
};

/// Every command Ariel registers with a chat platform.
pub const ALL: &[CommandSpec] = &[
    link::COMMAND,
    STATUS,
    SPAWN,
    KILL,
    RESPAWN,
    CHANNEL,
    CONFIGURE,
    INVITE,
];

/// How long an invite's token stays redeemable when nobody says otherwise.
const DEFAULT_INVITE_HOURS: u32 = 24;

/// What the commands need to answer.
#[derive(Debug, Clone)]
pub struct Context {
    pub records: Records,
    pub prospero: ProsperoClient,
    /// Linked from a status reply and a spawned agent.
    pub dashboard: Option<Url>,
    /// Whether this provider can answer someone privately — ephemerally or by
    /// direct message (ADR 0006). `/ariel invite` refuses to mint a token that
    /// could only be delivered in public.
    pub private_replies: bool,
}

/// Answer one command.
pub async fn respond(context: &Context, command: &Command) {
    let (reply, visibility) = match command.name.as_str() {
        name if name == link::COMMAND.name => {
            // Linking replies for itself, always privately.
            link::respond(&context.records, command, now_ms()).await;
            return;
        }
        name if name == STATUS.name => status(context, command).await,
        name if name == SPAWN.name => spawn(context, command).await,
        name if name == KILL.name => on_agent(context, command, &KILL).await,
        name if name == RESPAWN.name => on_agent(context, command, &RESPAWN).await,
        name if name == CHANNEL.name => channel(context, command).await,
        name if name == CONFIGURE.name => configure(context, command).await,
        name if name == INVITE.name => invite(context, command).await,
        other => (
            warning(format!("`/ariel {other}` is not a command Ariel knows.")),
            Visibility::Private,
        ),
    };
    if let Err(error) = command.responder.reply(&reply, visibility).await {
        tracing::warn!(%error, command = %command.name, "could not answer a command");
    }
}

async fn status(context: &Context, command: &Command) -> (Message, Visibility) {
    let channel = command.at.channel();
    let request = Request {
        user: &command.user,
        channel,
        command: &STATUS,
        workspace: None,
        surface_ref: None,
    };
    match auth::authorize(&context.records, &request).await {
        Err(error) => return internal_failure(&error, "status"),
        Ok(Decision::Denied(denial)) => return (denied(&denial, &STATUS), Visibility::Private),
        Ok(Decision::Allowed { .. }) => {}
    }

    // Authorization has just read this record, so it is there unless an admin
    // removed it in between; then the channel follows nothing.
    let follows = match ChannelConfig::key_for(
        channel.provider.as_str(),
        channel.tenant.as_str(),
        &channel.channel,
    ) {
        Ok(key) => match context.records.get::<ChannelConfig>(&key).await {
            Ok(config) => config.map(|config| config.value.follows),
            Err(error) => return internal_failure(&error, "status"),
        },
        Err(error) => return internal_failure(&error, "status"),
    };
    let Some(follows) = follows else {
        return (
            denied(&Denial::ChannelNotConfigured, &STATUS),
            Visibility::Private,
        );
    };

    match context.prospero.fleet().await {
        Ok(fleet) => (
            render_status(&fleet, &follows, context.dashboard.as_ref()),
            Visibility::Public,
        ),
        Err(error) => (prospero_failure(&error, None), Visibility::Private),
    }
}

async fn spawn(context: &Context, command: &Command) -> (Message, Visibility) {
    let argument = |spec: &ArgSpec| {
        command
            .args
            .get(spec.name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let (Some(workspace), Some(prompt)) = (argument(&WORKSPACE_ARG), argument(&PROMPT_ARG)) else {
        return (
            warning("Give a workspace and a prompt: `/ariel spawn <workspace> <prompt>`."),
            Visibility::Private,
        );
    };

    let request = Request {
        user: &command.user,
        channel: command.at.channel(),
        command: &SPAWN,
        workspace: Some(workspace),
        surface_ref: None,
    };
    let decision = match auth::authorize(&context.records, &request).await {
        Err(error) => return internal_failure(&error, "spawn"),
        // Already audited by `authorize`.
        Ok(Decision::Denied(denial)) => return (denied(&denial, &SPAWN), Visibility::Private),
        Ok(decision) => decision,
    };

    let outcome = context
        .prospero
        .spawn(workspace, &SpawnRequest::new(prompt))
        .await;
    let result = match &outcome {
        Ok(_) => AuditResult::Succeeded,
        Err(error) => AuditResult::Failed(error.to_string()),
    };
    if let Err(error) = auth::record_outcome(&context.records, &request, &decision, result).await {
        // The agent's fate does not depend on the audit trail; say so loudly.
        tracing::error!(%error, workspace, "could not audit a spawn");
    }

    match outcome {
        Ok(spawned) => {
            let body = if spawned.created {
                format!(
                    "Agent `{}` is starting in `{}`.",
                    spawned.agent_id, spawned.workspace
                )
            } else {
                format!(
                    "An identical agent is already running in `{}`: `{}`.",
                    spawned.workspace, spawned.agent_id
                )
            };
            (
                Message {
                    severity: Severity::Success,
                    link: context.dashboard.clone(),
                    ..Message::text(body)
                },
                Visibility::Public,
            )
        }
        Err(error) => (
            prospero_failure(&error, Some(workspace)),
            Visibility::Private,
        ),
    }
}

/// `/ariel kill` and `/ariel respawn`, which differ only in what they ask
/// prosperod to do.
///
/// Both act on an agent rather than a workspace, and authorization is by
/// workspace ([ADR 0012](../../docs/adr/0012-command-authorization-and-audit.md)),
/// so the agent's workspace has to be resolved first. The order matters: the
/// channel's configuration is checked before anything is said about the fleet,
/// so an unconfigured channel cannot be used to find out which agents exist.
async fn on_agent(
    context: &Context,
    command: &Command,
    spec: &CommandSpec,
) -> (Message, Visibility) {
    let Some(agent) = command
        .args
        .get(AGENT_ARG.name)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            warning(format!("Name the agent: `/ariel {} <agent>`.", spec.name)),
            Visibility::Private,
        );
    };

    let channel = command.at.channel();
    // Checked before the fleet is read: an unconfigured channel must not be
    // able to find out which agents exist, so its refusal cannot depend on the
    // answer.
    match configured(context, channel).await {
        Err(error) => return internal_failure(&error, spec.name),
        Ok(false) => {
            return (
                denied(&Denial::ChannelNotConfigured, spec),
                Visibility::Private,
            );
        }
        Ok(true) => {}
    }

    let workspace = match context.prospero.fleet().await {
        Err(error) => return (prospero_failure(&error, None), Visibility::Private),
        Ok(fleet) => match fleet.agents().find(|known| known.id == agent) {
            Some(known) => known.workspace.clone(),
            None => {
                return (
                    warning(format!("No agent called `{agent}` is in the fleet.")),
                    Visibility::Private,
                );
            }
        },
    };

    let request = Request {
        user: &command.user,
        channel,
        command: spec,
        workspace: Some(&workspace),
        surface_ref: None,
    };
    let decision = match auth::authorize(&context.records, &request).await {
        Err(error) => return internal_failure(&error, spec.name),
        // Already audited by `authorize`.
        Ok(Decision::Denied(denial)) => return (denied(&denial, spec), Visibility::Private),
        Ok(decision) => decision,
    };

    let outcome = if spec.name == KILL.name {
        context
            .prospero
            .kill(agent)
            .await
            .map(|()| format!("Killing `{agent}` in `{workspace}`."))
    } else {
        context.prospero.respawn(agent).await.map(|respawned| {
            format!(
                "Respawned `{agent}` in `{workspace}` as `{}`.",
                respawned.agent_id
            )
        })
    };
    let result = match &outcome {
        Ok(_) => AuditResult::Succeeded,
        Err(error) => AuditResult::Failed(error.to_string()),
    };
    if let Err(error) = auth::record_outcome(&context.records, &request, &decision, result).await {
        tracing::error!(%error, command = spec.name, agent, "could not audit a command");
    }

    match outcome {
        Ok(body) => (
            Message {
                severity: Severity::Success,
                link: context.dashboard.clone(),
                ..Message::text(body)
            },
            Visibility::Public,
        ),
        Err(error) => (prospero_failure(&error, None), Visibility::Private),
    }
}

/// `/ariel channel`: what this channel follows, hears and allows.
async fn channel(context: &Context, command: &Command) -> (Message, Visibility) {
    let channel = command.at.channel();
    let request = Request {
        user: &command.user,
        channel,
        command: &CHANNEL,
        workspace: None,
        surface_ref: None,
    };
    match auth::authorize(&context.records, &request).await {
        Err(error) => return internal_failure(&error, CHANNEL.name),
        Ok(Decision::Denied(denial)) => return (denied(&denial, &CHANNEL), Visibility::Private),
        Ok(Decision::Allowed { .. }) => {}
    }

    match channels::show(&context.records, &key_of(channel)).await {
        Err(error) => internal_failure(&error, CHANNEL.name),
        Ok(None) => (
            denied(&Denial::ChannelNotConfigured, &CHANNEL),
            Visibility::Private,
        ),
        Ok(Some(config)) => (
            Message {
                title: Some("Channel configuration".to_owned()),
                fields: vec![
                    (
                        "follows".to_owned(),
                        channels::follows_line(&config.follows),
                    ),
                    (
                        "hears".to_owned(),
                        channels::notify_name(config.notify).to_owned(),
                    ),
                    (
                        "ceiling".to_owned(),
                        channels::role_name(config.ceiling).to_owned(),
                    ),
                ],
                ..Message::default()
            },
            Visibility::Public,
        ),
    }
}

/// `/ariel configure`: change this channel's configuration (admin, audited).
async fn configure(context: &Context, command: &Command) -> (Message, Visibility) {
    let channel = command.at.channel();
    let argument = |spec: &ArgSpec| {
        command
            .args
            .get(spec.name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };

    let mut patch = channels::ChannelPatch::default();
    if let Some(spec) = argument(&FOLLOWS_ARG) {
        match channels::parse_follows(spec) {
            Ok(follows) => patch.follows = Some(follows),
            Err(error) => return (warning(format!("`follows`: {error}.")), Visibility::Private),
        }
    }
    if let Some(spec) = argument(&NOTIFY_ARG) {
        match channels::parse_notify(spec) {
            Some(notify) => patch.notify = Some(notify),
            None => {
                return (
                    warning("`notify` is `all`, `terminal` or `failures`."),
                    Visibility::Private,
                );
            }
        }
    }
    if let Some(spec) = argument(&CEILING_ARG) {
        match channels::parse_role(spec) {
            Some(ceiling) => patch.ceiling = Some(ceiling),
            None => {
                return (
                    warning("`ceiling` is `viewer`, `operator` or `admin`."),
                    Visibility::Private,
                );
            }
        }
    }
    if patch == channels::ChannelPatch::default() {
        return (
            warning(
                "Name something to change: `follows`, `notify` or `ceiling`. \
                 `/ariel channel` shows what is set.",
            ),
            Visibility::Private,
        );
    }

    let request = Request {
        user: &command.user,
        channel,
        command: &CONFIGURE,
        workspace: None,
        surface_ref: None,
    };
    let decision = match auth::authorize(&context.records, &request).await {
        Err(error) => return internal_failure(&error, CONFIGURE.name),
        Ok(Decision::Denied(denial)) => return (denied(&denial, &CONFIGURE), Visibility::Private),
        Ok(decision) => decision,
    };
    let Decision::Allowed { person, .. } = &decision else {
        unreachable!("a denial has already returned");
    };

    // A channel with no record cannot be configured from inside itself: there
    // is nothing yet saying commands are allowed here. `ariel channel set`
    // bootstraps it.
    let applied = channels::set(
        &context.records,
        &key_of(channel),
        patch,
        &FleetActor::Person(person.clone()),
    )
    .await;
    let result = match &applied {
        Ok(_) => AuditResult::Succeeded,
        Err(error) => AuditResult::Failed(error.to_string()),
    };
    if let Err(error) = auth::record_outcome(&context.records, &request, &decision, result).await {
        tracing::error!(%error, "could not audit a channel change");
    }

    match applied {
        Err(error) => internal_failure(&error, CONFIGURE.name),
        Ok(channels::Applied::Unchanged(_)) => (
            Message::text("That is already how this channel is configured."),
            Visibility::Private,
        ),
        Ok(channels::Applied::Conflict(_)) => (
            warning(
                "Someone else changed this channel first; `/ariel channel` shows what is stored now.",
            ),
            Visibility::Private,
        ),
        Ok(
            channels::Applied::Created(config) | channels::Applied::Updated { after: config, .. },
        ) => (
            Message {
                title: Some("Channel configuration".to_owned()),
                severity: Severity::Success,
                fields: vec![
                    (
                        "follows".to_owned(),
                        channels::follows_line(&config.follows),
                    ),
                    (
                        "hears".to_owned(),
                        channels::notify_name(config.notify).to_owned(),
                    ),
                    (
                        "ceiling".to_owned(),
                        channels::role_name(config.ceiling).to_owned(),
                    ),
                ],
                ..Message::default()
            },
            Visibility::Public,
        ),
    }
}

/// `/ariel invite`: mint a one-time link token (admin, audited).
///
/// The reply carries a live secret, so it is private or it does not happen:
/// on a provider that can neither reply ephemerally nor send a direct message,
/// nothing is minted at all, rather than minting a token Ariel cannot hand over
/// safely.
async fn invite(context: &Context, command: &Command) -> (Message, Visibility) {
    let argument = |spec: &ArgSpec| {
        command
            .args
            .get(spec.name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };

    let Some(role) = argument(&ROLE_ARG).and_then(channels::parse_role) else {
        return (
            warning("Say which role: `/ariel invite role:<viewer|operator|admin>`."),
            Visibility::Private,
        );
    };
    let workspace = argument(&WORKSPACE_ONLY_ARG).map(str::to_owned);
    let hours = match argument(&HOURS_ARG) {
        None => DEFAULT_INVITE_HOURS,
        Some(value) => match value.parse::<u32>().ok().filter(|hours| *hours > 0) {
            Some(hours) => hours,
            None => {
                return (
                    warning("`hours` is a whole number of hours above zero."),
                    Visibility::Private,
                );
            }
        },
    };

    if !context.private_replies {
        return (
            warning(
                "This chat platform cannot answer privately, and a link token must not be \
                 posted in a channel. Mint it with `ariel link new` instead.",
            ),
            Visibility::Private,
        );
    }

    let request = Request {
        user: &command.user,
        channel: command.at.channel(),
        command: &INVITE,
        workspace: workspace.as_deref(),
        surface_ref: None,
    };
    let decision = match auth::authorize(&context.records, &request).await {
        Err(error) => return internal_failure(&error, INVITE.name),
        Ok(Decision::Denied(denial)) => return (denied(&denial, &INVITE), Visibility::Private),
        Ok(decision) => decision,
    };
    let Decision::Allowed { person, effective } = &decision else {
        unreachable!("a denial has already returned");
    };

    // Nobody hands out more than they hold.
    if as_role(role) > *effective {
        let reply = warning(format!(
            "You act as **{}** here, so you cannot grant **{}**.",
            role_name(*effective),
            channels::role_name(role)
        ));
        if let Err(error) = auth::record_outcome(
            &context.records,
            &request,
            &decision,
            AuditResult::Failed("refused to grant above the inviter's own role".to_owned()),
        )
        .await
        {
            tracing::error!(%error, "could not audit a refused invite");
        }
        return (reply, Visibility::Private);
    }

    let scope = match &workspace {
        Some(workspace) => GrantScope::Workspace(workspace.clone()),
        None => GrantScope::Fleet,
    };
    let minted = link::mint(
        &context.records,
        link::MintRequest {
            role,
            scope: scope.clone(),
            person: None,
            ttl: Duration::from_secs(u64::from(hours) * 3600),
            minted_by: FleetActor::Person(person.clone()),
        },
        now_ms(),
    )
    .await;
    let result = match &minted {
        Ok(_) => AuditResult::Succeeded,
        Err(error) => AuditResult::Failed(error.to_string()),
    };
    if let Err(error) = auth::record_outcome(&context.records, &request, &decision, result).await {
        tracing::error!(%error, "could not audit an invite");
    }

    match minted {
        Err(error) => {
            tracing::error!(%error, "could not mint a link token");
            (
                Message {
                    severity: Severity::Failure,
                    ..Message::text("Minting the token failed on our side; try again shortly.")
                },
                Visibility::Private,
            )
        }
        // Private: this is the one reply that carries a secret.
        Ok(minted) => (
            Message {
                severity: Severity::Success,
                ..Message::text(format!(
                    "Give this to the person, privately. It works once, expires in {hours}h, \
                     and grants **{}** {}:\n\n`/ariel link {}`",
                    channels::role_name(role),
                    scope_phrase(&scope),
                    minted.token
                ))
            },
            Visibility::Private,
        ),
    }
}

/// A stored role as the role a command runs with, so the two can be compared.
fn as_role(role: FleetRole) -> Role {
    match role {
        FleetRole::Viewer => Role::Viewer,
        FleetRole::Operator => Role::Operator,
        FleetRole::Admin => Role::Admin,
    }
}

/// How a grant's scope reads in a reply.
fn scope_phrase(scope: &GrantScope) -> String {
    match scope {
        GrantScope::Fleet => "across the fleet".to_owned(),
        GrantScope::Workspace(name) => format!("in workspace `{name}`"),
    }
}

fn key_of(channel: &ChannelRef) -> channels::ChannelKey {
    channels::ChannelKey::new(
        channel.provider.as_str(),
        channel.tenant.as_str(),
        &channel.channel,
    )
}

/// Whether this channel has a configuration record at all.
async fn configured(context: &Context, channel: &ChannelRef) -> Result<bool, AuthError> {
    let key = ChannelConfig::key_for(
        channel.provider.as_str(),
        channel.tenant.as_str(),
        &channel.channel,
    )?;
    Ok(context.records.get::<ChannelConfig>(&key).await?.is_some())
}

/// A fleet summary, limited to the workspaces `follows` names.
#[must_use]
pub fn render_status(fleet: &FleetSnapshot, follows: &Follows, dashboard: Option<&Url>) -> Message {
    let mut workspaces: Vec<_> = fleet
        .workspaces
        .iter()
        .filter(|workspace| match follows {
            Follows::Fleet => true,
            Follows::Workspaces { names } => names.contains(&workspace.name),
        })
        .collect();
    workspaces.sort_by(|a, b| a.name.cmp(&b.name));

    let active = workspaces
        .iter()
        .flat_map(|workspace| &workspace.agents)
        .filter(|agent| !agent.status.is_terminal())
        .count();
    let unreachable = workspaces
        .iter()
        .any(|workspace| matches!(workspace.health, WorkspaceHealth::Unreachable { .. }));

    let mut lines = Vec::with_capacity(workspaces.len() + 1);
    lines.push(if workspaces.is_empty() {
        "prosperod knows none of the workspaces this channel follows.".to_owned()
    } else {
        format!(
            "{} across {}.",
            plural(active, "active agent"),
            plural(workspaces.len(), "workspace")
        )
    });
    for workspace in &workspaces {
        let detail = match &workspace.health {
            WorkspaceHealth::Unreachable { reason } => format!("unreachable: {reason}"),
            WorkspaceHealth::Healthy | WorkspaceHealth::Unknown => agent_counts(&workspace.agents),
        };
        lines.push(format!("- **{}**: {detail}", workspace.name));
    }

    Message {
        title: Some("Fleet status".to_owned()),
        body: lines.join("\n"),
        severity: if unreachable {
            Severity::Warning
        } else {
            Severity::Info
        },
        link: dashboard.cloned(),
        ..Message::default()
    }
}

/// `2 running, 1 idle`, in lifecycle order, or `no agents`.
fn agent_counts(agents: &[crate::prospero::types::Agent]) -> String {
    let mut counts: BTreeMap<u8, (&str, usize)> = BTreeMap::new();
    for agent in agents {
        let (order, label) = status_label(agent.status);
        counts.entry(order).or_insert((label, 0)).1 += 1;
    }
    if counts.is_empty() {
        return "no agents".to_owned();
    }
    counts
        .values()
        .map(|(label, count)| format!("{count} {label}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn status_label(status: AgentStatus) -> (u8, &'static str) {
    match status {
        AgentStatus::Spawning => (0, "spawning"),
        AgentStatus::Running => (1, "running"),
        AgentStatus::Idle => (2, "idle"),
        AgentStatus::Done => (3, "done"),
        AgentStatus::Failed => (4, "failed"),
        AgentStatus::Crashed => (5, "crashed"),
        AgentStatus::Killed => (6, "killed"),
        AgentStatus::Unknown => (7, "in an unknown state"),
    }
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Why the command was refused, and what would change that.
fn denied(denial: &Denial, command: &CommandSpec) -> Message {
    let name = command.name;
    let body = match denial {
        Denial::ChannelNotConfigured => format!(
            "This channel isn't set up for Ariel, so `/ariel {name}` doesn't work here. \
             An admin can configure it with `ariel channel set`."
        ),
        Denial::WorkspaceNotFollowed { workspace } => format!(
            "This channel doesn't follow workspace `{workspace}`, so commands here can't act on it."
        ),
        Denial::Unlinked => "Your chat account isn't linked to a fleet identity yet. \
             Ask an admin for a link token, then run `/ariel link <token>`."
            .to_owned(),
        Denial::NoRole if command.min_role == Role::Viewer => format!(
            "You hold no fleet-wide role, which `/ariel {name}` needs. \
             Ask an admin for one."
        ),
        Denial::NoRole => {
            format!("You hold no role that covers `/ariel {name}` here. Ask an admin for one.")
        }
        Denial::InsufficientRole {
            effective,
            required,
        } => format!(
            "`/ariel {name}` needs **{}**, and in this channel you act as **{}**.",
            role_name(*required),
            role_name(*effective)
        ),
    };
    warning(body)
}

/// A prosperod failure, as a sentence for the person who asked.
fn prospero_failure(error: &ClientError, workspace: Option<&str>) -> Message {
    let body = match error {
        ClientError::Api { kind, .. }
            if kind.as_deref() == Some("not_found") && workspace.is_some() =>
        {
            format!(
                "prosperod doesn't know a workspace called `{}`.",
                workspace.unwrap_or_default()
            )
        }
        ClientError::Api {
            status, message, ..
        } if status.is_client_error() => format!("prosperod refused the request: {message}"),
        ClientError::Api { message, .. } => {
            tracing::error!(%error, "prosperod failed a command");
            format!("prosperod failed to carry it out: {message}")
        }
        ClientError::Auth { .. } => {
            tracing::error!(
                %error,
                "prosperod refused Ariel's token; set ARIEL_PROSPERO_TOKEN_FILE to a valid token"
            );
            "prosperod refused Ariel's own credentials, so Ariel can't act on the fleet \
             right now. An admin needs to check Ariel's prosperod token."
                .to_owned()
        }
        ClientError::Transport(_) => {
            tracing::warn!(%error, "could not reach prosperod for a command");
            "Ariel couldn't reach prosperod; try again shortly.".to_owned()
        }
        ClientError::Json(_) | ClientError::BaseUrl { .. } => {
            tracing::error!(%error, "unexpected prosperod failure");
            "Something went wrong talking to prosperod; try again shortly.".to_owned()
        }
    };
    Message {
        severity: Severity::Failure,
        ..Message::text(body)
    }
}

/// A failure on Ariel's side, such as gonzalod being unreachable.
fn internal_failure(error: &dyn std::fmt::Display, command: &str) -> (Message, Visibility) {
    tracing::error!(%error, command, "a command failed");
    (
        Message {
            severity: Severity::Failure,
            ..Message::text(format!(
                "`/ariel {command}` failed on Ariel's side; try again shortly."
            ))
        },
        Visibility::Private,
    )
}

fn warning(body: impl Into<String>) -> Message {
    Message {
        severity: Severity::Warning,
        ..Message::text(body)
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Viewer => "viewer",
        Role::Operator => "operator",
        Role::Admin => "admin",
    }
}
