//! Turning fleet events into chat notifications (#19, ADR 0007 and ADR 0009).
//!
//! One [`Notifier`] runs per configured channel. It folds an agent's events
//! into an [`AgentView`], keeps one live message per agent, and edits it in
//! place as the agent progresses. A burst of spawns shares one summary message
//! instead of flooding the channel, and sends are paced against the provider's
//! own budget.
//!
//! Nothing here is persisted ([ADR 0003](../../docs/adr/0003-no-state-of-its-own.md)):
//! a restarted daemon starts a fresh live message for each running agent and
//! does not replay what it missed.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::chat::{ChannelRef, ChatProvider, Destination, Message, MessageRef, ProviderError, Url};
use crate::prospero::types::FleetEvent;
use crate::records::gonzalo::{ChannelConfig, Follows, NotifyPreset};
use crate::render::{AgentView, render_agent, render_summary};

/// Which events a channel hears, and where they go (ADR 0009).
#[derive(Debug, Clone)]
pub struct Route {
    channel: ChannelRef,
    destination: Destination,
    follows: Follows,
    notify: NotifyPreset,
}

impl Route {
    pub fn new(channel: ChannelRef, follows: Follows, notify: NotifyPreset) -> Self {
        Self {
            destination: Destination::Channel(channel.clone()),
            channel,
            follows,
            notify,
        }
    }

    /// The route a stored channel configuration describes.
    pub fn from_config(config: &ChannelConfig) -> Self {
        Self::new(
            ChannelRef::new(&config.provider, &config.tenant, &config.channel),
            config.follows.clone(),
            config.notify,
        )
    }

    pub fn channel(&self) -> &ChannelRef {
        &self.channel
    }

    pub fn destination(&self) -> &Destination {
        &self.destination
    }

    pub fn notify(&self) -> NotifyPreset {
        self.notify
    }

    /// Whether events from `workspace` reach this channel.
    pub fn follows_workspace(&self, workspace: &str) -> bool {
        match &self.follows {
            Follows::Fleet => true,
            Follows::Workspaces { names } => names.contains(workspace),
        }
    }

    /// Whether this agent's state is one the preset notifies on. Presets only
    /// narrow ADR 0007's defaults; they never add event kinds.
    fn admits(&self, view: &AgentView) -> bool {
        match self.notify {
            NotifyPreset::All => true,
            NotifyPreset::Terminal => view.is_terminal(),
            NotifyPreset::Failures => view.is_failure() || view.gone,
        }
    }
}

/// Timings and thresholds for a [`Notifier`]. The defaults are ADR 0007's.
#[derive(Debug, Clone)]
pub struct NotifyConfig {
    /// How long a new agent waits before its first send, so a lone agent can be
    /// told apart from a burst.
    pub hold: Duration,
    /// How many agents arriving together share a summary instead of each
    /// getting their own message.
    pub burst_threshold: usize,
    /// A spawn this soon after a summary's latest member joins that summary.
    pub burst_window: Duration,
    /// The least time between two edits of the same message.
    pub min_edit_interval: Duration,
    /// How long a channel stays unhealthy after the platform refuses a send.
    pub unhealthy_recheck: Duration,
    /// First backoff after a transport failure or an unqualified rate limit.
    pub backoff_initial: Duration,
    /// Longest backoff after repeated failures.
    pub backoff_max: Duration,
    /// Linked from every message, when the fleet has a dashboard.
    pub dashboard: Option<Url>,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            hold: Duration::from_secs(2),
            burst_threshold: 5,
            burst_window: Duration::from_secs(10),
            min_edit_interval: Duration::from_secs(2),
            unhealthy_recheck: Duration::from_secs(300),
            backoff_initial: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            dashboard: None,
        }
    }
}

/// Notifies one channel about the fleet.
pub struct Notifier {
    provider: Arc<dyn ChatProvider>,
    route: Route,
    config: NotifyConfig,
}

impl Notifier {
    pub fn new(provider: Arc<dyn ChatProvider>, route: Route, config: NotifyConfig) -> Self {
        Self {
            provider,
            route,
            config,
        }
    }

    /// Start notifying. Send fleet events to the returned sender; dropping it
    /// stops the notifier.
    pub fn spawn(self, buffer: usize) -> mpsc::Sender<FleetEvent> {
        let (tx, rx) = mpsc::channel(buffer);
        tokio::spawn(run(self.provider, self.route, self.config, rx));
        tx
    }
}

/// What one agent's live message currently shows, and what it still owes.
#[derive(Debug)]
struct AgentSlot {
    view: AgentView,
    /// The live message, once posted.
    message: Option<MessageRef>,
    /// The state changed since the last successful send.
    dirty: bool,
    last_sent: Option<Instant>,
    /// Waiting out the hold, not yet sent at all.
    pending_since: Option<Instant>,
    /// Counted by the workspace summary instead of having its own message.
    in_summary: bool,
    /// A provider that cannot edit has already posted this agent's end.
    end_posted: bool,
    /// This agent must have a message of its own and must never be folded back
    /// into a summary: it failed (ADR 0007).
    solo: bool,
}

impl AgentSlot {
    /// Not yet notifiable: the hold starts when the preset first admits it, so
    /// a `terminal` or `failures` channel never holds a running agent.
    fn new(view: AgentView) -> Self {
        Self {
            view,
            message: None,
            dirty: false,
            last_sent: None,
            pending_since: None,
            in_summary: false,
            end_posted: false,
            solo: false,
        }
    }
}

/// One message standing in for a burst of agents in a workspace.
#[derive(Debug)]
struct SummarySlot {
    message: Option<MessageRef>,
    members: Vec<String>,
    dirty: bool,
    last_sent: Option<Instant>,
    /// When the newest member joined, for the burst window.
    last_joined: Instant,
}

/// A token bucket over the provider's own send budget.
#[derive(Debug)]
struct Budget {
    tokens: f64,
    burst: f64,
    /// Seconds one token takes to refill.
    interval: f64,
    last_refill: Instant,
}

impl Budget {
    fn new(burst: u32, per_hour: u32, now: Instant) -> Self {
        let burst = f64::from(burst.max(1));
        Self {
            tokens: burst,
            burst,
            interval: 3600.0 / f64::from(per_hour.max(1)),
            last_refill: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        if elapsed <= 0.0 {
            return;
        }
        self.tokens = (self.tokens + elapsed / self.interval).min(self.burst);
        self.last_refill = now;
    }

    /// Take one token, or say when the next one arrives.
    fn take(&mut self, now: Instant) -> Result<(), Instant> {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let wait = (1.0 - self.tokens) * self.interval;
        Err(now + Duration::from_secs_f64(wait))
    }
}

/// One thing to send now.
enum Send {
    Post { agent: String },
    Edit { agent: String },
    PostSummary { workspace: String },
    EditSummary { workspace: String },
}

struct State {
    agents: HashMap<String, AgentSlot>,
    summaries: HashMap<String, SummarySlot>,
    budget: Budget,
    /// Set while the platform is throttling us or has refused access.
    blocked_until: Option<Instant>,
    backoff: Duration,
    can_edit: bool,
}

async fn run(
    provider: Arc<dyn ChatProvider>,
    route: Route,
    config: NotifyConfig,
    mut rx: mpsc::Receiver<FleetEvent>,
) {
    let now = Instant::now();
    let budget = provider.capabilities().limits.send_budget;
    let mut state = State {
        agents: HashMap::new(),
        summaries: HashMap::new(),
        budget: Budget::new(budget.burst, budget.per_hour, now),
        blocked_until: None,
        backoff: config.backoff_initial,
        can_edit: provider.capabilities().edit,
    };

    loop {
        let deadline = next_deadline(&state, &config);
        let wait = async {
            match deadline {
                Some(at) => tokio::time::sleep_until(at).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            event = rx.recv() => match event {
                Some(event) => ingest(&mut state, &route, event),
                None => break,
            },
            () = wait => {}
        }
        step(&mut state, &route, &config, provider.as_ref()).await;
    }
}

/// Fold one event into the agent it belongs to.
fn ingest(state: &mut State, route: &Route, event: FleetEvent) {
    if event.agent_id.is_empty() || !route.follows_workspace(&event.repo) {
        return;
    }
    let now = Instant::now();
    let slot = state
        .agents
        .entry(event.agent_id.clone())
        .or_insert_with(|| AgentSlot::new(AgentView::new(&event.repo, &event.agent_id)));

    if !slot.view.apply(&event) {
        return;
    }
    if !route.admits(&slot.view) {
        return;
    }
    // A preset that only notifies at the end starts its hold then, not at
    // spawn.
    if slot.message.is_none() && !slot.in_summary && slot.pending_since.is_none() && !slot.dirty {
        slot.pending_since = Some(now);
    }
    if slot.view.is_failure() && !slot.solo {
        // A failure never hides inside a count: it leaves the summary for a post
        // of its own, without waiting out another hold. It stays in the
        // summary's membership, so the counts still include it.
        slot.solo = true;
        slot.in_summary = false;
        slot.message = None;
        slot.pending_since = None;
    }
    slot.dirty = true;

    // A summarized agent has no message of its own; its change is a change to
    // the summary's counts.
    let workspace = slot.view.workspace.clone();
    let summarized = slot.in_summary;
    if let Some(summary) = state.summaries.get_mut(&workspace)
        && (summarized || summary.members.contains(&event.agent_id))
    {
        summary.dirty = true;
    }
}

/// When the notifier next has something to do.
fn next_deadline(state: &State, config: &NotifyConfig) -> Option<Instant> {
    if let Some(blocked) = state.blocked_until {
        return Some(blocked);
    }
    let mut earliest: Option<Instant> = None;
    let mut note = |at: Instant| {
        earliest = Some(earliest.map_or(at, |current: Instant| current.min(at)));
    };

    for slot in state.agents.values() {
        if let Some(since) = slot.pending_since {
            note(since + config.hold);
        } else if slot.dirty {
            match slot.last_sent {
                Some(sent) => note(sent + config.min_edit_interval),
                None => note(Instant::now()),
            }
        }
    }
    for summary in state.summaries.values() {
        if summary.dirty {
            match summary.last_sent {
                Some(sent) => note(sent + config.min_edit_interval),
                None => note(Instant::now()),
            }
        }
    }
    earliest
}

/// Promote whatever is ready, then send at most one message.
async fn step(
    state: &mut State,
    route: &Route,
    config: &NotifyConfig,
    provider: &dyn ChatProvider,
) {
    let now = Instant::now();
    if let Some(blocked) = state.blocked_until {
        if now < blocked {
            return;
        }
        state.blocked_until = None;
    }
    promote(state, config, now);

    let Some(send) = choose(state, config, now) else {
        return;
    };
    if let Err(next_token) = state.budget.take(now) {
        state.blocked_until = Some(next_token);
        return;
    }
    perform(state, route, config, provider, send, now).await;
}

/// Move agents whose hold has elapsed into a summary or their own message.
fn promote(state: &mut State, config: &NotifyConfig, now: Instant) {
    let mut ready: HashMap<String, Vec<String>> = HashMap::new();
    for (id, slot) in &state.agents {
        if slot.solo {
            continue;
        }
        if slot
            .pending_since
            .is_some_and(|since| now >= since + config.hold)
        {
            ready
                .entry(slot.view.workspace.clone())
                .or_default()
                .push(id.clone());
        }
    }

    for (workspace, mut ids) in ready {
        ids.sort();
        let joins_existing = state
            .summaries
            .get(&workspace)
            .is_some_and(|summary| now <= summary.last_joined + config.burst_window);
        let summarized = joins_existing || ids.len() >= config.burst_threshold;

        if summarized {
            let summary = state
                .summaries
                .entry(workspace)
                .or_insert_with(|| SummarySlot {
                    message: None,
                    members: Vec::new(),
                    dirty: false,
                    last_sent: None,
                    last_joined: now,
                });
            for id in &ids {
                if !summary.members.contains(id) {
                    summary.members.push(id.clone());
                }
            }
            summary.last_joined = now;
            summary.dirty = true;
        }
        for id in ids {
            if let Some(slot) = state.agents.get_mut(&id) {
                slot.pending_since = None;
                slot.in_summary = summarized;
                slot.dirty = true;
            }
        }
    }
}

/// The next send, highest priority first: terminal states, then new messages,
/// then intermediate edits (ADR 0007).
fn choose(state: &State, config: &NotifyConfig, now: Instant) -> Option<Send> {
    let sendable = |slot: &AgentSlot| {
        if !slot.dirty || slot.pending_since.is_some() || slot.in_summary {
            return false;
        }
        if slot.message.is_some() && !state.can_edit {
            // Without edit support an agent gets one post at spawn and one at
            // the end, nothing between.
            return slot.view.is_terminal() && !slot.end_posted;
        }
        match slot.last_sent {
            Some(sent) if slot.message.is_some() => now >= sent + config.min_edit_interval,
            _ => true,
        }
    };

    let mut terminal: Vec<&String> = Vec::new();
    let mut fresh: Vec<&String> = Vec::new();
    let mut edits: Vec<&String> = Vec::new();
    for (id, slot) in &state.agents {
        if !sendable(slot) {
            continue;
        }
        if slot.view.is_terminal() {
            terminal.push(id);
        } else if slot.message.is_none() {
            fresh.push(id);
        } else {
            edits.push(id);
        }
    }
    for list in [&mut terminal, &mut fresh, &mut edits] {
        list.sort();
    }

    if let Some(id) = terminal.first() {
        let slot = &state.agents[*id];
        return Some(if slot.message.is_some() && state.can_edit {
            Send::Edit {
                agent: (*id).clone(),
            }
        } else {
            Send::Post {
                agent: (*id).clone(),
            }
        });
    }
    if let Some(id) = fresh.first() {
        return Some(Send::Post {
            agent: (*id).clone(),
        });
    }

    let mut summaries: Vec<(&String, &SummarySlot)> = state
        .summaries
        .iter()
        .filter(|(_, summary)| {
            summary.dirty
                && match summary.last_sent {
                    Some(sent) if summary.message.is_some() => {
                        state.can_edit && now >= sent + config.min_edit_interval
                    }
                    _ => true,
                }
        })
        .collect();
    summaries.sort_by_key(|(workspace, _)| (*workspace).clone());
    if let Some((workspace, summary)) = summaries.first() {
        return Some(if summary.message.is_some() {
            Send::EditSummary {
                workspace: (*workspace).clone(),
            }
        } else {
            Send::PostSummary {
                workspace: (*workspace).clone(),
            }
        });
    }

    edits.first().map(|id| Send::Edit {
        agent: (*id).clone(),
    })
}

/// Render the message a send carries, from the state as it is now.
fn render(state: &State, config: &NotifyConfig, send: &Send) -> Option<Message> {
    let dashboard = config.dashboard.as_ref();
    match send {
        Send::Post { agent } | Send::Edit { agent } => {
            Some(render_agent(&state.agents.get(agent)?.view, dashboard))
        }
        Send::PostSummary { workspace } | Send::EditSummary { workspace } => {
            let summary = state.summaries.get(workspace)?;
            let members: Vec<AgentView> = summary
                .members
                .iter()
                .filter_map(|id| state.agents.get(id).map(|slot| slot.view.clone()))
                .collect();
            Some(render_summary(workspace, &members, dashboard))
        }
    }
}

async fn perform(
    state: &mut State,
    route: &Route,
    config: &NotifyConfig,
    provider: &dyn ChatProvider,
    send: Send,
    now: Instant,
) {
    let Some(message) = render(state, config, &send) else {
        return;
    };

    let result = match &send {
        Send::Post { .. } | Send::PostSummary { .. } => {
            provider.post(route.destination(), &message).await.map(Some)
        }
        Send::Edit { agent } => match state.agents.get(agent).and_then(|s| s.message.clone()) {
            Some(target) => provider.edit(&target, &message).await.map(|()| None),
            None => return,
        },
        Send::EditSummary { workspace } => {
            match state
                .summaries
                .get(workspace)
                .and_then(|s| s.message.clone())
            {
                Some(target) => provider.edit(&target, &message).await.map(|()| None),
                None => return,
            }
        }
    };

    match result {
        Ok(posted) => {
            state.backoff = config.backoff_initial;
            match &send {
                Send::Post { agent } | Send::Edit { agent } => {
                    if let Some(slot) = state.agents.get_mut(agent) {
                        if let Some(message_ref) = posted {
                            slot.message = Some(message_ref);
                        }
                        if slot.view.is_terminal() {
                            slot.end_posted = true;
                        }
                        slot.dirty = false;
                        slot.last_sent = Some(now);
                    }
                }
                Send::PostSummary { workspace } | Send::EditSummary { workspace } => {
                    if let Some(summary) = state.summaries.get_mut(workspace) {
                        if let Some(message_ref) = posted {
                            summary.message = Some(message_ref);
                        }
                        summary.dirty = false;
                        summary.last_sent = Some(now);
                    }
                }
            }
        }
        Err(error) => handle_error(state, config, &send, error, now),
    }
}

fn handle_error(
    state: &mut State,
    config: &NotifyConfig,
    send: &Send,
    error: ProviderError,
    now: Instant,
) {
    match error {
        // The message was deleted: forget it and post a new one.
        ProviderError::NotFound => {
            match send {
                Send::Edit { agent } => {
                    if let Some(slot) = state.agents.get_mut(agent) {
                        slot.message = None;
                        slot.last_sent = None;
                    }
                }
                Send::EditSummary { workspace } => {
                    if let Some(summary) = state.summaries.get_mut(workspace) {
                        summary.message = None;
                        summary.last_sent = None;
                    }
                }
                // The channel itself is gone; treat it as lost access.
                Send::Post { .. } | Send::PostSummary { .. } => {
                    state.blocked_until = Some(now + config.unhealthy_recheck);
                }
            }
            tracing::debug!("chat message vanished; a new one will be posted");
        }
        // The bot lost access: stop sending until the re-check.
        ProviderError::Forbidden => {
            state.blocked_until = Some(now + config.unhealthy_recheck);
            tracing::warn!(
                recheck_in = ?config.unhealthy_recheck,
                "chat platform refused the send; pausing this channel"
            );
        }
        ProviderError::RateLimited { retry_after } => {
            let wait = retry_after.unwrap_or(state.backoff);
            state.blocked_until = Some(now + wait);
            state.backoff = (state.backoff * 2).min(config.backoff_max);
            tracing::debug!(?wait, "chat platform is throttling; pausing this channel");
        }
        // A message the core should have kept inside the provider's limits:
        // dropping it is better than retrying it forever.
        ProviderError::InvalidMessage(detail) => {
            clear(state, send);
            tracing::error!(%detail, "chat provider rejected a message");
        }
        ProviderError::Unsupported(what) => {
            clear(state, send);
            tracing::debug!(what, "chat provider does not support this send");
        }
        // Transport failures, and any variant a later provider adds, back off
        // and try the same state again.
        other => {
            state.blocked_until = Some(now + state.backoff);
            state.backoff = (state.backoff * 2).min(config.backoff_max);
            tracing::warn!(error = %other, "chat send failed; backing off");
        }
    }
}

/// Give up on this send without retrying it.
fn clear(state: &mut State, send: &Send) {
    match send {
        Send::Post { agent } | Send::Edit { agent } => {
            if let Some(slot) = state.agents.get_mut(agent) {
                slot.dirty = false;
            }
        }
        Send::PostSummary { workspace } | Send::EditSummary { workspace } => {
            if let Some(summary) = state.summaries.get_mut(workspace) {
                summary.dirty = false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_budget_refills_at_its_hourly_rate() {
        let start = Instant::now();
        let mut budget = Budget::new(2, 3600, start);
        assert!(budget.take(start).is_ok());
        assert!(budget.take(start).is_ok());

        let Err(next) = budget.take(start) else {
            panic!("a third send should exhaust a burst of two");
        };
        assert_eq!(next - start, Duration::from_secs(1));

        // One token per second at 3600 an hour.
        let later = start + Duration::from_secs(1);
        assert!(budget.take(later).is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_budget_never_banks_more_than_its_burst() {
        let start = Instant::now();
        let mut budget = Budget::new(3, 3600, start);
        budget.refill(start + Duration::from_secs(3600));
        assert!((budget.tokens - 3.0).abs() < f64::EPSILON);
    }
}
