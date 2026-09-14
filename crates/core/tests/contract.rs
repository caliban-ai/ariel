//! The shared provider contract suite (ADR 0006), checked against
//! `ConsoleProvider` and against deliberately broken providers.

use std::sync::Arc;

use ariel_core::chat::console::ConsoleProvider;
use ariel_core::chat::contract::{self, Harness, Violation};
use ariel_core::chat::{
    Args, Capabilities, ChatProvider, CommandSpec, Destination, Inbound, Message, MessageRef,
    ProviderError, ProviderId, ThreadRef, UserRef, Visibility,
};
use async_trait::async_trait;
use futures_util::stream::BoxStream;

fn destination() -> Destination {
    Destination::Channel(ariel_core::chat::ChannelRef::new("console", "t1", "ops"))
}

fn user() -> UserRef {
    UserRef::new("console", "t1", "u1")
}

/// Injects commands into a console the way a platform would deliver them.
struct ConsoleHarness {
    console: Arc<ConsoleProvider>,
    deliver: bool,
}

impl Harness for ConsoleHarness {
    fn destination(&self) -> Destination {
        destination()
    }

    fn user(&self) -> UserRef {
        user()
    }

    fn inject_command(&self, name: &str, args: Args) {
        if self.deliver {
            self.console
                .inject_command(user(), destination(), name, args);
        }
    }
}

fn harness(console: &Arc<ConsoleProvider>) -> ConsoleHarness {
    ConsoleHarness {
        console: Arc::clone(console),
        deliver: true,
    }
}

#[tokio::test]
async fn console_with_every_capability_satisfies_the_contract() {
    let console = Arc::new(ConsoleProvider::new());

    assert_eq!(
        contract::check(console.as_ref(), &harness(&console)).await,
        []
    );
}

#[tokio::test]
async fn console_with_no_optional_capability_satisfies_the_contract() {
    let console = Arc::new(ConsoleProvider::with_capabilities(Capabilities::new(
        ConsoleProvider::LIMITS,
    )));

    assert_eq!(
        contract::check(console.as_ref(), &harness(&console)).await,
        []
    );
}

/// A provider that advertises `capabilities` while behaving like `inner`, and
/// can be told to misbehave in specific ways.
struct Broken {
    inner: Arc<ConsoleProvider>,
    capabilities: Capabilities,
    reuse_refs: bool,
    leak_private_replies: bool,
}

impl Broken {
    fn new(inner: &Arc<ConsoleProvider>, capabilities: Capabilities) -> Self {
        Self {
            inner: Arc::clone(inner),
            capabilities,
            reuse_refs: false,
            leak_private_replies: false,
        }
    }
}

/// Answers private replies publicly, ignoring the platform's capability.
struct LeakyResponder;

#[async_trait]
impl ariel_core::chat::Responder for LeakyResponder {
    async fn defer(&self, _visibility: Visibility) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn reply(
        &self,
        _msg: &Message,
        _visibility: Visibility,
    ) -> Result<MessageRef, ProviderError> {
        Ok(MessageRef {
            at: destination(),
            message: "leaked".to_owned(),
        })
    }
}

#[async_trait]
impl ChatProvider for Broken {
    fn id(&self) -> ProviderId {
        ProviderId::new("broken")
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }

    async fn register_commands(&self, specs: &[CommandSpec]) -> Result<(), ProviderError> {
        self.inner.register_commands(specs).await
    }

    async fn post(&self, to: &Destination, msg: &Message) -> Result<MessageRef, ProviderError> {
        let posted = self.inner.post(to, msg).await?;
        if self.reuse_refs {
            return Ok(MessageRef {
                at: posted.at,
                message: "same".to_owned(),
            });
        }
        Ok(posted)
    }

    fn inbound(&self) -> BoxStream<'static, Inbound> {
        use futures_util::StreamExt;
        let leak = self.leak_private_replies;
        self.inner
            .inbound()
            .map(move |item| match item {
                Inbound::Command(mut command) if leak => {
                    command.responder = Box::new(LeakyResponder);
                    Inbound::Command(command)
                }
                other => other,
            })
            .boxed()
    }

    async fn edit(&self, target: &MessageRef, msg: &Message) -> Result<(), ProviderError> {
        self.inner.edit(target, msg).await
    }

    async fn direct_message(
        &self,
        user: &UserRef,
        msg: &Message,
    ) -> Result<MessageRef, ProviderError> {
        self.inner.direct_message(user, msg).await
    }

    async fn start_thread(
        &self,
        root: &MessageRef,
        title: &str,
    ) -> Result<ThreadRef, ProviderError> {
        self.inner.start_thread(root, title).await
    }
}

#[tokio::test]
async fn advertising_an_unsupported_edit_is_flagged() {
    let inner = Arc::new(ConsoleProvider::with_capabilities(Capabilities::new(
        ConsoleProvider::LIMITS,
    )));
    let broken = Broken::new(
        &inner,
        Capabilities::new(ConsoleProvider::LIMITS).with_edit(true),
    );

    let violations = contract::check(&broken, &harness(&inner)).await;

    assert_eq!(
        violations,
        [Violation::CapabilityMismatch {
            capability: "edit",
            advertised: true,
        }]
    );
}

#[tokio::test]
async fn hiding_a_supported_direct_message_is_flagged() {
    let inner = Arc::new(ConsoleProvider::new());
    let broken = Broken::new(
        &inner,
        Capabilities::all(ConsoleProvider::LIMITS).with_direct_messages(false),
    );

    let violations = contract::check(&broken, &harness(&inner)).await;

    assert_eq!(
        violations,
        [Violation::CapabilityMismatch {
            capability: "direct_messages",
            advertised: false,
        }]
    );
}

#[tokio::test]
async fn reused_message_refs_are_flagged() {
    let inner = Arc::new(ConsoleProvider::with_capabilities(Capabilities::new(
        ConsoleProvider::LIMITS,
    )));
    let mut broken = Broken::new(&inner, Capabilities::new(ConsoleProvider::LIMITS));
    broken.reuse_refs = true;

    let violations = contract::check(&broken, &harness(&inner)).await;

    assert_eq!(violations, [Violation::DuplicateMessageRefs]);
}

#[tokio::test]
async fn an_undelivered_command_is_flagged() {
    let console = Arc::new(ConsoleProvider::new());
    let silent = ConsoleHarness {
        console: Arc::clone(&console),
        deliver: false,
    };

    let violations = contract::check(console.as_ref(), &silent).await;

    assert_eq!(violations, [Violation::CommandNotDelivered]);
}

#[tokio::test]
async fn accepting_a_private_reply_without_ephemeral_support_is_flagged() {
    let inner = Arc::new(ConsoleProvider::with_capabilities(Capabilities::new(
        ConsoleProvider::LIMITS,
    )));
    let mut broken = Broken::new(&inner, Capabilities::new(ConsoleProvider::LIMITS));
    broken.leak_private_replies = true;

    let violations = contract::check(&broken, &harness(&inner)).await;

    assert_eq!(violations, [Violation::PrivateReplyAccepted]);
}
