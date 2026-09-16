//! Shared helpers for the notifier tests: a provider whose failures a test can
//! script, and builders for the fleet events the notifier reacts to.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use ariel_core::chat::console::{ConsoleProvider, Recorded};
use ariel_core::chat::{
    Capabilities, ChatProvider, CommandSpec, Destination, Inbound, Message, MessageRef,
    ProviderError, ProviderId,
};
use ariel_core::prospero::types::{AgentStatus, EventKind, FleetEvent};
use async_trait::async_trait;
use futures_util::stream::BoxStream;

/// A [`ConsoleProvider`] whose next post or edit a test can turn into a
/// failure, and which counts every attempt, including the failed ones.
#[derive(Debug)]
pub struct ScriptedProvider {
    console: ConsoleProvider,
    capabilities: Capabilities,
    attempts: AtomicU64,
    next_post_error: Mutex<Option<ProviderError>>,
    next_edit_error: Mutex<Option<ProviderError>>,
}

impl ScriptedProvider {
    pub fn new() -> Self {
        Self::with_capabilities(Capabilities::all(ConsoleProvider::LIMITS))
    }

    /// A provider that cannot edit, so the notifier must fall back to posts.
    pub fn without_edit() -> Self {
        Self::with_capabilities(Capabilities::all(ConsoleProvider::LIMITS).with_edit(false))
    }

    fn with_capabilities(capabilities: Capabilities) -> Self {
        Self {
            console: ConsoleProvider::with_capabilities(capabilities),
            capabilities,
            attempts: AtomicU64::new(0),
            next_post_error: Mutex::new(None),
            next_edit_error: Mutex::new(None),
        }
    }

    /// Everything the provider was asked to do, in order.
    pub fn log(&self) -> Vec<Recorded> {
        self.console.log()
    }

    /// How many sends were attempted, successful or not.
    pub fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::SeqCst)
    }

    pub fn fail_next_post(&self, error: ProviderError) {
        *lock(&self.next_post_error) = Some(error);
    }

    pub fn fail_next_edit(&self, error: ProviderError) {
        *lock(&self.next_edit_error) = Some(error);
    }
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

#[async_trait]
impl ChatProvider for ScriptedProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("console")
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn register_commands(&self, specs: &[CommandSpec]) -> Result<(), ProviderError> {
        self.console.register_commands(specs).await
    }

    async fn post(&self, to: &Destination, msg: &Message) -> Result<MessageRef, ProviderError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = lock(&self.next_post_error).take() {
            return Err(error);
        }
        self.console.post(to, msg).await
    }

    fn inbound(&self) -> BoxStream<'static, Inbound> {
        self.console.inbound()
    }

    async fn edit(&self, target: &MessageRef, msg: &Message) -> Result<(), ProviderError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = lock(&self.next_edit_error).take() {
            return Err(error);
        }
        if !self.capabilities.edit {
            return Err(ProviderError::Unsupported("edit"));
        }
        self.console.edit(target, msg).await
    }
}

/// Sequence numbers are per-agent in prospero; tests only need them distinct.
static SEQ: AtomicU64 = AtomicU64::new(0);

fn event(workspace: &str, agent: &str, kind: EventKind) -> FleetEvent {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    FleetEvent {
        seq,
        ts: format!("2026-09-15T00:00:{:02}Z", seq % 60),
        repo: workspace.to_owned(),
        agent_id: agent.to_owned(),
        kind,
    }
}

pub fn spawned(workspace: &str, agent: &str) -> FleetEvent {
    event(workspace, agent, EventKind::AgentSpawned)
}

pub fn status(workspace: &str, agent: &str, to: AgentStatus) -> FleetEvent {
    event(
        workspace,
        agent,
        EventKind::StatusChanged {
            from: AgentStatus::Unknown,
            to,
        },
    )
}

pub fn crashed(workspace: &str, agent: &str) -> FleetEvent {
    status(workspace, agent, AgentStatus::Crashed)
}

pub fn finished(
    workspace: &str,
    agent: &str,
    outcome: &str,
    cost_usd: f64,
    turns: u32,
) -> FleetEvent {
    event(
        workspace,
        agent,
        EventKind::AgentFinished {
            outcome: outcome.to_owned(),
            cost_usd,
            turns,
        },
    )
}
