//! `ConsoleProvider`: an in-memory [`ChatProvider`] for tests.
//!
//! It records every outbound call in order, lets tests inject inbound
//! commands, and honours whatever [`Capabilities`] it is built with, so tests
//! can exercise the core's fallbacks without a real platform (ADR 0006).

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};
use tokio::sync::mpsc;

use super::{
    Args, Capabilities, ChannelRef, ChatProvider, Command, CommandSpec, Destination, Inbound,
    Limits, Message, MessageRef, ProviderError, ProviderId, Responder, SendBudget, ThreadRef,
    UserRef, Visibility,
};

/// One thing the console was asked to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    Posted {
        message_ref: MessageRef,
        message: Message,
    },
    Edited {
        message_ref: MessageRef,
        message: Message,
    },
    DirectMessage {
        user: UserRef,
        message_ref: MessageRef,
        message: Message,
    },
    ThreadStarted {
        root: MessageRef,
        title: String,
        thread: ThreadRef,
    },
    Deferred {
        visibility: Visibility,
    },
    Replied {
        visibility: Visibility,
        message_ref: MessageRef,
        message: Message,
    },
    CommandsRegistered(Vec<String>),
}

#[derive(Debug, Default)]
struct State {
    log: Vec<Recorded>,
    /// Every message the console has sent, so edits and threads can check the
    /// target exists.
    sent: HashSet<MessageRef>,
    next_id: u64,
}

impl State {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn send(&mut self, at: &Destination) -> MessageRef {
        let message_ref = MessageRef {
            at: at.clone(),
            message: format!("m-{}", self.next_id()),
        };
        self.sent.insert(message_ref.clone());
        message_ref
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    // A test that panicked mid-call must not hide the log from the next
    // assertion.
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An in-memory chat provider.
#[derive(Debug)]
pub struct ConsoleProvider {
    capabilities: Capabilities,
    state: Arc<Mutex<State>>,
    inbound_tx: mpsc::UnboundedSender<Inbound>,
    inbound_rx: Mutex<Option<mpsc::UnboundedReceiver<Inbound>>>,
}

impl ConsoleProvider {
    /// Generous limits, so tests exercise truncation only when they ask to.
    pub const LIMITS: Limits = Limits {
        title_chars: 256,
        body_chars: 4096,
        fields: 25,
        field_chars: 1024,
        actions: 25,
        total_chars: 6000,
        send_budget: SendBudget {
            burst: 5,
            per_hour: 3600,
        },
    };

    /// A console supporting every optional capability.
    pub fn new() -> Self {
        Self::with_capabilities(Capabilities::all(Self::LIMITS))
    }

    pub fn with_capabilities(capabilities: Capabilities) -> Self {
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel();
        Self {
            capabilities,
            state: Arc::default(),
            inbound_tx,
            inbound_rx: Mutex::new(Some(inbound_rx)),
        }
    }

    /// Deliver a command on the inbound stream, as if someone ran it.
    pub fn inject_command(&self, user: UserRef, at: Destination, name: &str, args: Args) {
        let responder = ConsoleResponder {
            state: Arc::clone(&self.state),
            capabilities: self.capabilities,
            at: at.clone(),
            deferred: AtomicBool::new(false),
        };
        let command = Command {
            name: name.to_owned(),
            args,
            user,
            at,
            responder: Box::new(responder),
        };
        // Sending fails only if the inbound stream was taken and dropped, in
        // which case nobody is listening for the command anyway.
        let _ = self.inbound_tx.send(Inbound::Command(command));
    }

    /// Everything recorded so far, in order.
    pub fn log(&self) -> Vec<Recorded> {
        lock(&self.state).log.clone()
    }
}

impl Default for ConsoleProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChatProvider for ConsoleProvider {
    fn id(&self) -> ProviderId {
        ProviderId::new("console")
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn register_commands(&self, specs: &[CommandSpec]) -> Result<(), ProviderError> {
        let names = specs.iter().map(|spec| spec.name.to_owned()).collect();
        lock(&self.state)
            .log
            .push(Recorded::CommandsRegistered(names));
        Ok(())
    }

    async fn post(&self, to: &Destination, msg: &Message) -> Result<MessageRef, ProviderError> {
        if matches!(to, Destination::Thread(_)) && !self.capabilities.threads {
            return Err(ProviderError::Unsupported("threads"));
        }
        let mut state = lock(&self.state);
        let message_ref = state.send(to);
        state.log.push(Recorded::Posted {
            message_ref: message_ref.clone(),
            message: msg.clone(),
        });
        Ok(message_ref)
    }

    /// Taken once: a second call returns an empty stream.
    fn inbound(&self) -> BoxStream<'static, Inbound> {
        let receiver = self
            .inbound_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match receiver {
            Some(receiver) => stream::unfold(receiver, |mut receiver| async move {
                receiver.recv().await.map(|item| (item, receiver))
            })
            .boxed(),
            None => stream::empty().boxed(),
        }
    }

    async fn edit(&self, target: &MessageRef, msg: &Message) -> Result<(), ProviderError> {
        if !self.capabilities.edit {
            return Err(ProviderError::Unsupported("edit"));
        }
        let mut state = lock(&self.state);
        if !state.sent.contains(target) {
            return Err(ProviderError::NotFound);
        }
        state.log.push(Recorded::Edited {
            message_ref: target.clone(),
            message: msg.clone(),
        });
        Ok(())
    }

    async fn direct_message(
        &self,
        user: &UserRef,
        msg: &Message,
    ) -> Result<MessageRef, ProviderError> {
        if !self.capabilities.direct_messages {
            return Err(ProviderError::Unsupported("direct_messages"));
        }
        let at = Destination::Channel(ChannelRef {
            provider: user.provider.clone(),
            tenant: user.tenant.clone(),
            channel: format!("dm:{}", user.user),
        });
        let mut state = lock(&self.state);
        let message_ref = state.send(&at);
        state.log.push(Recorded::DirectMessage {
            user: user.clone(),
            message_ref: message_ref.clone(),
            message: msg.clone(),
        });
        Ok(message_ref)
    }

    async fn start_thread(
        &self,
        root: &MessageRef,
        title: &str,
    ) -> Result<ThreadRef, ProviderError> {
        if !self.capabilities.threads {
            return Err(ProviderError::Unsupported("threads"));
        }
        let mut state = lock(&self.state);
        if !state.sent.contains(root) {
            return Err(ProviderError::NotFound);
        }
        let thread = ThreadRef {
            channel: root.at.channel().clone(),
            thread: format!("t-{}", state.next_id()),
        };
        state.log.push(Recorded::ThreadStarted {
            root: root.clone(),
            title: title.to_owned(),
            thread: thread.clone(),
        });
        Ok(thread)
    }
}

/// Answers one injected command, recording what it was asked to do.
struct ConsoleResponder {
    state: Arc<Mutex<State>>,
    capabilities: Capabilities,
    at: Destination,
    deferred: AtomicBool,
}

impl ConsoleResponder {
    fn check(&self, visibility: Visibility) -> Result<(), ProviderError> {
        if visibility == Visibility::Private && !self.capabilities.ephemeral_replies {
            return Err(ProviderError::Unsupported("ephemeral_replies"));
        }
        Ok(())
    }
}

#[async_trait]
impl Responder for ConsoleResponder {
    async fn defer(&self, visibility: Visibility) -> Result<(), ProviderError> {
        self.check(visibility)?;
        if !self.deferred.swap(true, Ordering::SeqCst) {
            lock(&self.state)
                .log
                .push(Recorded::Deferred { visibility });
        }
        Ok(())
    }

    async fn reply(
        &self,
        msg: &Message,
        visibility: Visibility,
    ) -> Result<MessageRef, ProviderError> {
        self.check(visibility)?;
        let mut state = lock(&self.state);
        let message_ref = state.send(&self.at);
        state.log.push(Recorded::Replied {
            visibility,
            message_ref: message_ref.clone(),
            message: msg.clone(),
        });
        Ok(message_ref)
    }
}
