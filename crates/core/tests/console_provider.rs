//! `ConsoleProvider` and the `ChatProvider` contract (ADR 0006).

use std::sync::Arc;

use ariel_core::chat::console::{ConsoleProvider, Recorded};
use ariel_core::chat::{
    ArgSpec, Args, Capabilities, ChannelRef, ChatProvider, Command, CommandSpec, Destination,
    Inbound, Limits, Message, MessageRef, ProviderError, ProviderId, Role, SendBudget, UserRef,
    Visibility,
};
use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};

fn limits() -> Limits {
    Limits {
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
    }
}

fn channel() -> ChannelRef {
    ChannelRef::new("console", "tenant-1", "ops")
}

fn user() -> UserRef {
    UserRef::new("console", "tenant-1", "user-1")
}

fn in_channel() -> Destination {
    Destination::Channel(channel())
}

async fn next_command(console: &ConsoleProvider) -> Command {
    let mut inbound = console.inbound();
    match inbound.next().await {
        Some(Inbound::Command(command)) => command,
        _ => panic!("expected an inbound command"),
    }
}

#[tokio::test]
async fn posts_are_recorded_in_order_with_distinct_refs() {
    let console = ConsoleProvider::new();

    let first = console
        .post(&in_channel(), &Message::text("a1 started"))
        .await
        .unwrap();
    let second = console
        .post(&in_channel(), &Message::text("a1 finished"))
        .await
        .unwrap();

    assert_eq!(first.at, in_channel());
    assert_ne!(first.message, second.message);
    assert_eq!(
        console.log(),
        [
            Recorded::Posted {
                message_ref: first,
                message: Message::text("a1 started"),
            },
            Recorded::Posted {
                message_ref: second,
                message: Message::text("a1 finished"),
            },
        ]
    );
}

#[tokio::test]
async fn injected_command_round_trips_through_inbound_and_reply() {
    let console = ConsoleProvider::new();
    console.inject_command(
        user(),
        in_channel(),
        "spawn",
        Args::from_pairs([("workspace", "caliban"), ("prompt", "fix the tests")]),
    );

    let command = next_command(&console).await;
    assert_eq!(command.name, "spawn");
    assert_eq!(command.user, user());
    assert_eq!(command.at, in_channel());
    assert_eq!(command.args.get("workspace"), Some("caliban"));
    assert_eq!(command.args.get("prompt"), Some("fix the tests"));
    assert_eq!(command.args.get("model"), None);

    let reply = command
        .responder
        .reply(&Message::text("spawned a9"), Visibility::Public)
        .await
        .unwrap();

    assert_eq!(reply.at, in_channel());
    assert_eq!(
        console.log(),
        [Recorded::Replied {
            visibility: Visibility::Public,
            message_ref: reply,
            message: Message::text("spawned a9"),
        }]
    );
}

#[tokio::test]
async fn deferring_twice_records_one_defer() {
    let console = ConsoleProvider::new();
    console.inject_command(user(), in_channel(), "status", Args::default());
    let command = next_command(&console).await;

    command.responder.defer(Visibility::Private).await.unwrap();
    command.responder.defer(Visibility::Private).await.unwrap();

    assert_eq!(
        console.log(),
        [Recorded::Deferred {
            visibility: Visibility::Private
        }]
    );
}

#[tokio::test]
async fn private_reply_is_refused_without_ephemeral_replies() {
    let console = ConsoleProvider::with_capabilities(Capabilities::new(limits()));
    console.inject_command(user(), in_channel(), "link", Args::default());
    let command = next_command(&console).await;

    let result = command
        .responder
        .reply(&Message::text("linked"), Visibility::Private)
        .await;

    assert!(matches!(
        result,
        Err(ProviderError::Unsupported("ephemeral_replies"))
    ));
    assert_eq!(console.log(), []);
}

#[tokio::test]
async fn private_reply_is_recorded_as_private_when_supported() {
    let caps = Capabilities::new(limits()).with_ephemeral_replies(true);
    let console = ConsoleProvider::with_capabilities(caps);
    console.inject_command(user(), in_channel(), "link", Args::default());
    let command = next_command(&console).await;

    let reply = command
        .responder
        .reply(&Message::text("linked"), Visibility::Private)
        .await
        .unwrap();

    assert_eq!(
        console.log(),
        [Recorded::Replied {
            visibility: Visibility::Private,
            message_ref: reply,
            message: Message::text("linked"),
        }]
    );
}

#[tokio::test]
async fn gated_methods_are_unsupported_when_capabilities_are_off() {
    let console = ConsoleProvider::with_capabilities(Capabilities::new(limits()));
    let posted = console
        .post(&in_channel(), &Message::text("root"))
        .await
        .unwrap();

    assert!(matches!(
        console.edit(&posted, &Message::text("edited")).await,
        Err(ProviderError::Unsupported("edit"))
    ));
    assert!(matches!(
        console.direct_message(&user(), &Message::text("hi")).await,
        Err(ProviderError::Unsupported("direct_messages"))
    ));
    assert!(matches!(
        console.start_thread(&posted, "a1").await,
        Err(ProviderError::Unsupported("threads"))
    ));
    assert_eq!(console.log().len(), 1, "only the post is recorded");
}

#[tokio::test]
async fn edit_direct_message_and_threads_are_recorded_when_supported() {
    let console = ConsoleProvider::new();
    let root = console
        .post(&in_channel(), &Message::text("a1 running"))
        .await
        .unwrap();

    console
        .edit(&root, &Message::text("a1 done"))
        .await
        .unwrap();
    let dm = console
        .direct_message(&user(), &Message::text("your token"))
        .await
        .unwrap();
    let thread = console.start_thread(&root, "a1 session").await.unwrap();
    let in_thread = console
        .post(
            &Destination::Thread(thread.clone()),
            &Message::text("hello"),
        )
        .await
        .unwrap();

    assert_eq!(thread.channel, channel());
    assert_eq!(
        console.log(),
        [
            Recorded::Posted {
                message_ref: root.clone(),
                message: Message::text("a1 running"),
            },
            Recorded::Edited {
                message_ref: root.clone(),
                message: Message::text("a1 done"),
            },
            Recorded::DirectMessage {
                user: user(),
                message_ref: dm,
                message: Message::text("your token"),
            },
            Recorded::ThreadStarted {
                root,
                title: "a1 session".to_owned(),
                thread,
            },
            Recorded::Posted {
                message_ref: in_thread,
                message: Message::text("hello"),
            },
        ]
    );
}

#[tokio::test]
async fn unknown_messages_are_not_found() {
    let console = ConsoleProvider::new();
    let never_posted = MessageRef {
        at: in_channel(),
        message: "m-404".to_owned(),
    };

    assert!(matches!(
        console.edit(&never_posted, &Message::text("x")).await,
        Err(ProviderError::NotFound)
    ));
    assert!(matches!(
        console.start_thread(&never_posted, "x").await,
        Err(ProviderError::NotFound)
    ));
    assert_eq!(console.log(), []);
}

#[tokio::test]
async fn registered_commands_are_recorded() {
    const SPECS: &[CommandSpec] = &[
        CommandSpec {
            name: "status",
            summary: "Show the fleet",
            args: &[],
            min_role: Role::Viewer,
        },
        CommandSpec {
            name: "spawn",
            summary: "Start an agent",
            args: &[
                ArgSpec {
                    name: "workspace",
                    summary: "Workspace to spawn in",
                    required: true,
                },
                ArgSpec {
                    name: "prompt",
                    summary: "What the agent should do",
                    required: true,
                },
            ],
            min_role: Role::Operator,
        },
    ];
    let console = ConsoleProvider::new();

    console.register_commands(SPECS).await.unwrap();

    assert_eq!(
        console.log(),
        [Recorded::CommandsRegistered(vec![
            "status".to_owned(),
            "spawn".to_owned()
        ])]
    );
}

/// A provider implementing only the required methods.
struct Minimal;

#[async_trait]
impl ChatProvider for Minimal {
    fn id(&self) -> ProviderId {
        ProviderId::new("minimal")
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::new(limits())
    }

    async fn register_commands(&self, _specs: &[CommandSpec]) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn post(&self, to: &Destination, _msg: &Message) -> Result<MessageRef, ProviderError> {
        Ok(MessageRef {
            at: to.clone(),
            message: "m-1".to_owned(),
        })
    }

    fn inbound(&self) -> BoxStream<'static, Inbound> {
        stream::empty().boxed()
    }
}

#[tokio::test]
async fn gated_methods_default_to_unsupported() {
    let provider: Arc<dyn ChatProvider> = Arc::new(Minimal);
    let root = provider
        .post(&in_channel(), &Message::text("x"))
        .await
        .unwrap();

    assert!(matches!(
        provider.edit(&root, &Message::text("y")).await,
        Err(ProviderError::Unsupported("edit"))
    ));
    assert!(matches!(
        provider.direct_message(&user(), &Message::text("y")).await,
        Err(ProviderError::Unsupported("direct_messages"))
    ));
    assert!(matches!(
        provider.start_thread(&root, "t").await,
        Err(ProviderError::Unsupported("threads"))
    ));
}
