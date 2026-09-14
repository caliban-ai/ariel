//! Rendering fleet events into chat messages (#13, ADR 0007).
//!
//! Ariel posts one live message per agent and edits it as the agent
//! progresses, so rendering works on an agent's *current state*, not on
//! individual events. [`AgentView`] folds events into that state and reports
//! whether the live message needs updating; [`render_agent`] and
//! [`render_summary`] turn state into provider-neutral [`Message`]s.

use crate::chat::{Message, Severity, Url};
use crate::prospero::types::{AgentStatus, EventKind, FleetEvent};

/// How an agent finished, from its `agent_finished` event.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// Free text, e.g. `success` or `max_turns`.
    pub outcome: String,
    pub cost_usd: f64,
    pub turns: u32,
}

impl Outcome {
    fn succeeded(&self) -> bool {
        self.outcome == "success"
    }
}

/// Everything an agent's live message shows.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentView {
    pub workspace: String,
    pub agent_id: String,
    pub name: Option<String>,
    pub status: AgentStatus,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub outcome: Option<Outcome>,
    pub gone: bool,
}

impl AgentView {
    /// A newly seen agent, not yet spawned.
    pub fn new(workspace: impl Into<String>, agent_id: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            agent_id: agent_id.into(),
            name: None,
            status: AgentStatus::Spawning,
            started_at: None,
            ended_at: None,
            outcome: None,
            gone: false,
        }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Fold one event in. Returns whether the live message changed.
    ///
    /// Only the kinds ADR 0007 notifies on change the view: spawned, status
    /// changes, finished and gone. Output, tool, init, discovery, persistence,
    /// health and unknown events never do.
    pub fn apply(&mut self, event: &FleetEvent) -> bool {
        match &event.kind {
            EventKind::AgentSpawned => {
                if self.started_at.is_some() {
                    return false;
                }
                self.started_at = Some(event.ts.clone());
                true
            }
            EventKind::StatusChanged { to, .. } => {
                if *to == self.status {
                    return false;
                }
                self.status = *to;
                if to.is_terminal() {
                    self.mark_ended(&event.ts);
                }
                true
            }
            EventKind::AgentFinished {
                outcome,
                cost_usd,
                turns,
            } => {
                self.outcome = Some(Outcome {
                    outcome: outcome.clone(),
                    cost_usd: *cost_usd,
                    turns: *turns,
                });
                self.mark_ended(&event.ts);
                true
            }
            EventKind::AgentGone => {
                if self.gone {
                    return false;
                }
                self.gone = true;
                self.mark_ended(&event.ts);
                true
            }
            _ => false,
        }
    }

    /// The agent will not change again.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal() || self.outcome.is_some() || self.gone
    }

    /// The agent ended failed or crashed.
    pub fn is_failure(&self) -> bool {
        matches!(self.status, AgentStatus::Failed | AgentStatus::Crashed)
    }

    fn mark_ended(&mut self, ts: &str) {
        if self.ended_at.is_none() {
            self.ended_at = Some(ts.to_owned());
        }
    }

    /// Stopped short of finishing: killed, or gone before any outcome.
    fn is_stopped(&self) -> bool {
        !self.is_failure()
            && (self.status == AgentStatus::Killed
                || (self.gone && self.outcome.is_none() && self.status != AgentStatus::Done))
    }

    fn label(&self) -> &'static str {
        if self.status.is_terminal() {
            return status_label(self.status);
        }
        if self.gone {
            return "gone";
        }
        if self.outcome.is_some() {
            return "finished";
        }
        status_label(self.status)
    }

    fn severity(&self) -> Severity {
        if self.is_failure() {
            return Severity::Failure;
        }
        if self.status == AgentStatus::Killed {
            return Severity::Warning;
        }
        match &self.outcome {
            Some(outcome) if outcome.succeeded() => Severity::Success,
            None if self.status == AgentStatus::Done => Severity::Success,
            Some(_) => Severity::Warning,
            None if self.gone => Severity::Warning,
            None => Severity::Info,
        }
    }
}

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Spawning => "spawning",
        AgentStatus::Running => "running",
        AgentStatus::Idle => "idle",
        AgentStatus::Killed => "killed",
        AgentStatus::Done => "done",
        AgentStatus::Failed => "failed",
        AgentStatus::Crashed => "crashed",
        AgentStatus::Unknown => "unknown",
    }
}

/// An agent's live message.
pub fn render_agent(view: &AgentView, dashboard: Option<&Url>) -> Message {
    let name = view.name.as_deref().unwrap_or(&view.agent_id);

    let mut fields = Vec::new();
    if let Some(started) = &view.started_at {
        fields.push(("started".to_owned(), started.clone()));
    }
    if let Some(ended) = &view.ended_at {
        fields.push(("ended".to_owned(), ended.clone()));
    }
    if let Some(outcome) = &view.outcome {
        fields.push(("outcome".to_owned(), outcome.outcome.clone()));
        fields.push(("cost".to_owned(), format!("${:.2}", outcome.cost_usd)));
        fields.push(("turns".to_owned(), outcome.turns.to_string()));
    }

    Message {
        title: Some(format!("{} · {name}", view.workspace)),
        body: view.label().to_owned(),
        fields,
        severity: view.severity(),
        link: dashboard.cloned(),
        actions: Vec::new(),
    }
}

/// One message standing in for a burst of agents in a workspace.
///
/// Members that failed still get their own posts (ADR 0007); the summary only
/// counts them.
pub fn render_summary(workspace: &str, members: &[AgentView], dashboard: Option<&Url>) -> Message {
    let failed = members.iter().filter(|m| m.is_failure()).count();
    let stopped = members.iter().filter(|m| m.is_stopped()).count();
    let done = members
        .iter()
        .filter(|m| !m.is_failure() && !m.is_stopped() && m.is_terminal())
        .count();
    let running = members.len() - failed - stopped - done;

    let severity = if running > 0 {
        Severity::Info
    } else if failed == 0 && stopped == 0 {
        Severity::Success
    } else {
        Severity::Warning
    };

    Message {
        title: Some(format!("{workspace} · {} agents", members.len())),
        body: format!("{done} done · {running} running · {failed} failed · {stopped} stopped"),
        fields: Vec::new(),
        severity,
        link: dashboard.cloned(),
        actions: Vec::new(),
    }
}
