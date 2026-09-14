//! The chat provider boundary (ADR 0006).
//!
//! Every chat platform sits behind [`ChatProvider`]. The core talks only to
//! this trait: a small required surface, plus capability-gated methods that a
//! backend implements only when its platform supports them. [`Capabilities`]
//! says which, so the core decides fallbacks in one place instead of branching
//! on the provider.

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::BoxStream;

pub use reqwest::Url;

pub mod console;
#[cfg(feature = "contract-tests")]
pub mod contract;

/// Which chat platform, e.g. `discord`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A Discord guild, Slack workspace or Teams tenant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(String);

impl TenantId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A channel, scoped to its provider and tenant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChannelRef {
    pub provider: ProviderId,
    pub tenant: TenantId,
    /// The platform's channel ID.
    pub channel: String,
}

impl ChannelRef {
    pub fn new(
        provider: impl Into<String>,
        tenant: impl Into<String>,
        channel: impl Into<String>,
    ) -> Self {
        Self {
            provider: ProviderId::new(provider),
            tenant: TenantId::new(tenant),
            channel: channel.into(),
        }
    }
}

/// A person's chat account, scoped to its provider and tenant.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserRef {
    pub provider: ProviderId,
    pub tenant: TenantId,
    /// A user ID stable enough for account linking: the Discord user ID, the
    /// Slack user ID, or the Teams Entra object ID, never a per-bot ID.
    pub user: String,
}

impl UserRef {
    pub fn new(
        provider: impl Into<String>,
        tenant: impl Into<String>,
        user: impl Into<String>,
    ) -> Self {
        Self {
            provider: ProviderId::new(provider),
            tenant: TenantId::new(tenant),
            user: user.into(),
        }
    }
}

/// A thread within a channel. The thread ID is opaque and platform-specific.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadRef {
    pub channel: ChannelRef,
    pub thread: String,
}

/// Where a message goes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Destination {
    Channel(ChannelRef),
    Thread(ThreadRef),
}

impl Destination {
    /// The channel this destination is in.
    pub fn channel(&self) -> &ChannelRef {
        match self {
            Destination::Channel(channel) => channel,
            Destination::Thread(thread) => &thread.channel,
        }
    }
}

/// A message a provider has sent, usable to edit it or start a thread from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MessageRef {
    pub at: Destination,
    /// The platform's message ID.
    pub message: String,
}

/// How a message should read at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Severity {
    #[default]
    Info,
    Success,
    Warning,
    Failure,
}

/// A button on a message. Rendered only when `Capabilities::buttons` is set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Action {
    /// Returned with the click.
    pub id: String,
    pub label: String,
}

/// A provider-neutral message, mapped by each backend to its platform's
/// rich format.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub title: Option<String>,
    /// Bold, italic, code, links and lists only.
    pub body: String,
    pub fields: Vec<(String, String)>,
    pub severity: Severity,
    pub link: Option<Url>,
    pub actions: Vec<Action>,
}

impl Message {
    /// A message with only a body.
    pub fn text(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            ..Self::default()
        }
    }
}

/// A person's role, and a channel's ceiling on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Operator,
    Admin,
}

/// One argument of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub required: bool,
}

/// A command, declared once in the core. Backends with typed commands register
/// it natively; the rest parse free text against it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub args: &'static [ArgSpec],
    pub min_role: Role,
}

/// A command's arguments, by name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args(Vec<(String, String)>);

impl Args {
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        Self(
            pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }

    /// The value of the named argument, if given.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Whether a reply is visible to the channel or only to the person who asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Visibility {
    Public,
    Private,
}

/// Answers one inbound command or click. The backend meets its platform's
/// response deadline, deferring on the core's behalf if needed.
#[async_trait]
pub trait Responder: Send + Sync {
    /// Acknowledge now and reply later. Calling it again does nothing extra.
    async fn defer(&self, visibility: Visibility) -> Result<(), ProviderError>;

    /// Reply. A private reply never falls back to a public one.
    async fn reply(
        &self,
        msg: &Message,
        visibility: Visibility,
    ) -> Result<MessageRef, ProviderError>;
}

/// A slash command someone ran.
pub struct Command {
    pub name: String,
    pub args: Args,
    pub user: UserRef,
    /// Where it was run.
    pub at: Destination,
    pub responder: Box<dyn Responder>,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Command")
            .field("name", &self.name)
            .field("args", &self.args)
            .field("user", &self.user)
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

/// A button someone clicked (layer 2, #6).
pub struct ActionClick {
    pub user: UserRef,
    pub message: MessageRef,
    pub action: String,
    pub responder: Box<dyn Responder>,
}

impl fmt::Debug for ActionClick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActionClick")
            .field("user", &self.user)
            .field("message", &self.message)
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}

/// A reply someone posted in a thread (layer 4, #7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadReply {
    pub user: UserRef,
    pub thread: ThreadRef,
    pub text: String,
}

/// Something arriving from the platform.
#[derive(Debug)]
#[non_exhaustive]
pub enum Inbound {
    Command(Command),
    Action(ActionClick),
    ThreadReply(ThreadReply),
}

/// A per-channel token bucket the core paces sends against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendBudget {
    pub burst: u32,
    pub per_hour: u32,
}

/// The largest message a provider can send, and how fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub title_chars: usize,
    pub body_chars: usize,
    pub fields: usize,
    pub field_chars: usize,
    pub actions: usize,
    pub total_chars: usize,
    pub send_budget: SendBudget,
}

/// What a provider supports. The core checks these before calling a
/// capability-gated method, and picks a fallback when one is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Capabilities {
    pub edit: bool,
    pub direct_messages: bool,
    /// Discord allows these only in reply to an interaction.
    pub ephemeral_replies: bool,
    pub typed_commands: bool,
    pub commands_in_threads: bool,
    pub buttons: bool,
    pub threads: bool,
    pub reads_thread_replies: bool,
    pub limits: Limits,
}

impl Capabilities {
    /// Nothing optional supported.
    pub fn new(limits: Limits) -> Self {
        Self {
            edit: false,
            direct_messages: false,
            ephemeral_replies: false,
            typed_commands: false,
            commands_in_threads: false,
            buttons: false,
            threads: false,
            reads_thread_replies: false,
            limits,
        }
    }

    /// Everything optional supported.
    pub fn all(limits: Limits) -> Self {
        Self {
            edit: true,
            direct_messages: true,
            ephemeral_replies: true,
            typed_commands: true,
            commands_in_threads: true,
            buttons: true,
            threads: true,
            reads_thread_replies: true,
            limits,
        }
    }

    #[must_use]
    pub fn with_edit(mut self, on: bool) -> Self {
        self.edit = on;
        self
    }

    #[must_use]
    pub fn with_direct_messages(mut self, on: bool) -> Self {
        self.direct_messages = on;
        self
    }

    #[must_use]
    pub fn with_ephemeral_replies(mut self, on: bool) -> Self {
        self.ephemeral_replies = on;
        self
    }

    #[must_use]
    pub fn with_typed_commands(mut self, on: bool) -> Self {
        self.typed_commands = on;
        self
    }

    #[must_use]
    pub fn with_commands_in_threads(mut self, on: bool) -> Self {
        self.commands_in_threads = on;
        self
    }

    #[must_use]
    pub fn with_buttons(mut self, on: bool) -> Self {
        self.buttons = on;
        self
    }

    #[must_use]
    pub fn with_threads(mut self, on: bool) -> Self {
        self.threads = on;
        self
    }

    #[must_use]
    pub fn with_reads_thread_replies(mut self, on: bool) -> Self {
        self.reads_thread_replies = on;
        self
    }
}

/// Why a provider call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProviderError {
    /// The provider does not support this; the name matches the capability.
    #[error("not supported by this provider: {0}")]
    Unsupported(&'static str),
    /// The platform is throttling; longer waits are the caller's to schedule.
    #[error("rate limited by the chat platform")]
    RateLimited { retry_after: Option<Duration> },
    #[error("forbidden by the chat platform")]
    Forbidden,
    #[error("channel or message not found")]
    NotFound,
    /// Over a limit the core should have enforced: a bug.
    #[error("message rejected: {0}")]
    InvalidMessage(String),
    #[error("chat platform transport failed: {0}")]
    Transport(Box<dyn std::error::Error + Send + Sync>),
}

/// One chat platform.
#[async_trait]
pub trait ChatProvider: Send + Sync {
    fn id(&self) -> ProviderId;

    fn capabilities(&self) -> Capabilities;

    async fn register_commands(&self, specs: &[CommandSpec]) -> Result<(), ProviderError>;

    async fn post(&self, to: &Destination, msg: &Message) -> Result<MessageRef, ProviderError>;

    /// Commands, clicks and replies from the platform.
    fn inbound(&self) -> BoxStream<'static, Inbound>;

    /// Replace a sent message. Gated by `Capabilities::edit`.
    async fn edit(&self, target: &MessageRef, msg: &Message) -> Result<(), ProviderError> {
        let _ = (target, msg);
        Err(ProviderError::Unsupported("edit"))
    }

    /// Message a person directly. Gated by `Capabilities::direct_messages`.
    async fn direct_message(
        &self,
        user: &UserRef,
        msg: &Message,
    ) -> Result<MessageRef, ProviderError> {
        let _ = (user, msg);
        Err(ProviderError::Unsupported("direct_messages"))
    }

    /// Start a thread from a sent message. Gated by `Capabilities::threads`.
    async fn start_thread(
        &self,
        root: &MessageRef,
        title: &str,
    ) -> Result<ThreadRef, ProviderError> {
        let _ = (root, title);
        Err(ProviderError::Unsupported("threads"))
    }
}
