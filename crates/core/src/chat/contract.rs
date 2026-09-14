//! The shared provider contract suite (ADR 0006).
//!
//! Every backend runs [`check`] against a stub of its platform. The suite
//! returns the rules a provider breaks instead of panicking, so a backend's test
//! can assert on exactly what went wrong.

use std::time::Duration;

use futures_util::StreamExt;

use super::{
    Args, ChatProvider, Destination, Inbound, Message, ProviderError, UserRef, Visibility,
};

/// How long a command injected through the harness has to arrive.
const INBOUND_TIMEOUT: Duration = Duration::from_secs(2);

/// A rule of the `ChatProvider` contract a provider broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// Posting to the harness's destination failed, so nothing else was checked.
    PostFailed,
    /// A capability-gated method disagreed with the advertised flag: it returned
    /// `Unsupported` while advertised, or worked while not advertised.
    CapabilityMismatch {
        capability: &'static str,
        advertised: bool,
    },
    /// A post returned a ref at a different destination than requested.
    PostedElsewhere,
    /// Two posts returned the same message ID.
    DuplicateMessageRefs,
    /// A message within the advertised limits was rejected.
    MessageAtLimitsRejected,
    /// A command injected through the harness never arrived on `inbound`.
    CommandNotDelivered,
    /// A command arrived with the wrong name, user or destination.
    CommandMismatch,
    /// A public reply to a command failed or landed somewhere else.
    ReplyFailed,
    /// A private reply succeeded although ephemeral replies are not supported.
    PrivateReplyAccepted,
    /// A private reply failed although ephemeral replies are supported.
    PrivateReplyRefused,
}

/// The platform side of a contract run: where the suite posts, who it acts as,
/// and how it makes the platform deliver a command.
pub trait Harness: Send + Sync {
    fn destination(&self) -> Destination;

    fn user(&self) -> UserRef;

    /// Make the platform deliver a command, as if [`Harness::user`] ran it at
    /// [`Harness::destination`].
    fn inject_command(&self, name: &str, args: Args);
}

/// Run the contract suite against `provider`, returning every rule it breaks.
pub async fn check(provider: &dyn ChatProvider, harness: &dyn Harness) -> Vec<Violation> {
    let mut violations = Vec::new();
    let capabilities = provider.capabilities();
    let at = harness.destination();

    // Posting.
    let posted = (
        provider.post(&at, &Message::text("contract: first")).await,
        provider.post(&at, &Message::text("contract: second")).await,
    );
    let (first, second) = match posted {
        (Ok(first), Ok(second)) => (first, second),
        _ => {
            violations.push(Violation::PostFailed);
            return violations;
        }
    };
    if first.at != at || second.at != at {
        violations.push(Violation::PostedElsewhere);
    }
    if first.message == second.message {
        violations.push(Violation::DuplicateMessageRefs);
    }

    // Capability honesty.
    gate(
        &mut violations,
        "edit",
        capabilities.edit,
        provider
            .edit(&first, &Message::text("contract: edited"))
            .await,
    );
    gate(
        &mut violations,
        "direct_messages",
        capabilities.direct_messages,
        provider
            .direct_message(&harness.user(), &Message::text("contract: direct"))
            .await
            .map(|_| ()),
    );
    gate(
        &mut violations,
        "threads",
        capabilities.threads,
        provider.start_thread(&first, "contract").await.map(|_| ()),
    );

    // A message at the advertised limits must be accepted.
    let limits = capabilities.limits;
    let at_limits = Message::text("x".repeat(limits.body_chars.min(limits.total_chars)));
    if provider.post(&at, &at_limits).await.is_err() {
        violations.push(Violation::MessageAtLimitsRejected);
    }

    // Inbound commands and replies.
    let mut inbound = provider.inbound();
    harness.inject_command("status", Args::default());
    let command = match tokio::time::timeout(INBOUND_TIMEOUT, inbound.next()).await {
        Ok(Some(Inbound::Command(command))) => command,
        _ => {
            violations.push(Violation::CommandNotDelivered);
            return violations;
        }
    };
    if command.name != "status" || command.user != harness.user() || command.at != at {
        violations.push(Violation::CommandMismatch);
    }

    match command
        .responder
        .reply(&Message::text("contract: public reply"), Visibility::Public)
        .await
    {
        Ok(reply) if reply.at == command.at => {}
        _ => violations.push(Violation::ReplyFailed),
    }

    let private = command
        .responder
        .reply(
            &Message::text("contract: private reply"),
            Visibility::Private,
        )
        .await;
    match (capabilities.ephemeral_replies, private) {
        (false, Ok(_)) => violations.push(Violation::PrivateReplyAccepted),
        (true, Err(_)) => violations.push(Violation::PrivateReplyRefused),
        _ => {}
    }

    violations
}

/// Record a mismatch between a capability flag and how its method behaved.
fn gate(
    violations: &mut Vec<Violation>,
    capability: &'static str,
    advertised: bool,
    result: Result<(), ProviderError>,
) {
    let unsupported = matches!(result, Err(ProviderError::Unsupported(name)) if name == capability);
    if advertised == unsupported {
        violations.push(Violation::CapabilityMismatch {
            capability,
            advertised,
        });
    }
}
