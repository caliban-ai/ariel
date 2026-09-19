//! The `/ariel` commands and the router that answers them (#20, ADR 0012).
//!
//! - `/ariel link <token>` ties a chat account to a person ([`crate::link`]).
//! - `/ariel status` (viewer) summarizes the part of the fleet the channel
//!   follows. It acts on no one workspace, so it needs a fleet-wide grant.
//! - `/ariel spawn <workspace> <prompt>` (operator) starts an agent.
//!
//! Every command except `link` goes through [`auth::authorize`], which audits a
//! denied mutating command itself. The router records how an allowed one went
//! with [`auth::record_outcome`], so a spawn leaves exactly one audit entry
//! whichever way it ends.
//!
//! **Replies.** A result is public, so the channel sees the fleet and who
//! started what. A denial or failure is private to the person who asked: it
//! concerns only them, and a busy channel need not read it.

use std::collections::BTreeMap;

use crate::auth::{self, Decision, Denial, Request};
use crate::chat::{ArgSpec, Command, CommandSpec, Message, Role, Severity, Url, Visibility};
use crate::link;
use crate::prospero::types::{AgentStatus, FleetSnapshot, SpawnRequest, WorkspaceHealth};
use crate::prospero::{ClientError, ProsperoClient};
use crate::records::gonzalo::{AuditResult, ChannelConfig, Follows};
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

/// Every command Ariel registers with a chat platform.
pub const ALL: &[CommandSpec] = &[link::COMMAND, STATUS, SPAWN];

/// What the commands need to answer.
#[derive(Debug, Clone)]
pub struct Context {
    pub records: Records,
    pub prospero: ProsperoClient,
    /// Linked from a status reply and a spawned agent.
    pub dashboard: Option<Url>,
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
