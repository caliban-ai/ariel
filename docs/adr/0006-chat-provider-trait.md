# ADR 0006 · The `ChatProvider` trait: one core, capability-gated extras

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` (caliban-ai umbrella workspace) §Architecture, open question 8; issue #2

## Context

[ADR 0004](0004-provider-trait-and-feature-gated-backends.md) put every chat
platform behind one `ChatProvider` trait in `ariel-core` and left the trait's
shape to this decision. The risk is the one gonzalo's ADR 0010 names for ticket
systems: a trait shaped against one platform either flattens the others or grows
provider-specific escape hatches until it is no longer an abstraction.

We surveyed the official Discord, Slack and Microsoft Teams documentation
(verified 2026-09-13) for the primitives Ariel's four capability layers use.
They diverge exactly where Ariel needs them:

- **Response deadlines.** Discord requires a response within 3 s, then allows
  follow-ups for 15 minutes. Slack requires an ack within 3 s, then allows 5
  `response_url` uses within 30 minutes. Teams requires a response to an invoke
  within 5 s and has no follow-up token.
- **Commands.** Only Discord has typed slash-command options. Slack and Teams
  deliver free text, and Slack slash commands cannot be used inside threads.
- **Private replies.** Discord allows ephemeral messages only in reply to an
  interaction. Slack's are not guaranteed to be delivered. Teams' targeted
  messages expire after 24 hours.
- **Size.** Discord is tightest: 4096 characters of embed description and 6000
  across an embed. Slack recommends 4000 characters. Teams allows about 80 KB.
- **Threads.** Discord threads are channels that auto-archive. Slack threads are
  replies to a parent message. Teams threads are reply chains. Reading replies
  needs elevated access on every platform.
- **Throttling.** All three answer HTTP 429, each with different retry
  signalling.

The alternatives weighed for expressing what differs:

- **One trait with capability flags and `Unsupported` defaults**, as gonzalo's
  `TicketSource` does.
- **A minimal trait plus optional extension traits** (`Threads`, `Editing`)
  discovered through accessors. The type system then says what a backend
  supports, but there are more traits to wire and test, and a trait cannot say
  "supported, with a limit".
- **Every method required, with each backend degrading on its own.** This is the
  simplest for the core, but fallback policy scatters across backends and the
  core can neither know nor test what actually happened.

## Decision

We will define **one `ChatProvider` trait with a small required core,
capability-gated optional methods that default to `Unsupported`, and a
`Capabilities` descriptor the core consults before calling them.** The trait is
`#[async_trait]` and `Send + Sync`: the daemon holds providers as trait objects,
and native `async fn` in traits is not dyn-compatible on Rust 1.95.

### Addressing

Every reference names its provider and tenant (a Discord guild, Slack workspace
or Teams tenant). The MVP configures one tenant per provider, but serving several
later needs no change to the trait, and #3's channel configuration gets a key
that cannot collide across workspaces.

```rust
pub struct ChannelRef { pub provider: ProviderId, pub tenant: TenantId, pub channel: String }
pub struct UserRef    { pub provider: ProviderId, pub tenant: TenantId, pub user: String }
pub struct ThreadRef  { pub channel: ChannelRef, pub thread: String } // opaque per platform
pub enum Destination  { Channel(ChannelRef), Thread(ThreadRef) }
pub struct MessageRef { pub at: Destination, pub message: String }
```

`UserRef.user` must be stable for account linking: the Discord user ID, the Slack
user ID within `team_id` (or the Enterprise Grid user ID), or the Teams Entra
object ID within the tenant, never the per-bot `29:` ID.

### The trait

```rust
#[async_trait]
pub trait ChatProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn capabilities(&self) -> Capabilities;
    async fn register_commands(&self, specs: &[CommandSpec]) -> Result<(), ProviderError>;
    async fn post(&self, to: &Destination, msg: &Message) -> Result<MessageRef, ProviderError>;
    fn inbound(&self) -> BoxStream<'static, Inbound>;

    // Capability-gated; each defaults to Err(ProviderError::Unsupported).
    async fn edit(&self, target: &MessageRef, msg: &Message) -> Result<(), ProviderError>;
    async fn direct_message(&self, user: &UserRef, msg: &Message) -> Result<MessageRef, ProviderError>;
    async fn start_thread(&self, root: &MessageRef, title: &str) -> Result<ThreadRef, ProviderError>;
}

#[non_exhaustive]
pub enum Inbound {
    Command(Command),         // layer 3
    Action(ActionClick),      // layer 2
    ThreadReply(ThreadReply), // layer 4
}
```

How the backend receives events (a gateway socket, Socket Mode, or an HTTPS
endpoint) is its own concern; the trait assumes neither a socket nor a webhook.

### Messages

The core hands backends one provider-neutral `Message`, which each maps to a
Discord embed, Slack Block Kit or a Teams Adaptive Card:

```rust
pub struct Message {
    pub title: Option<String>,
    pub body: String,                 // bold, italic, code, links and lists only
    pub fields: Vec<(String, String)>,
    pub severity: Severity,           // Info | Success | Warning | Failure
    pub link: Option<Url>,
    pub actions: Vec<Action>,         // rendered only if capabilities().buttons
}
```

### Commands

The core declares each command once as a `CommandSpec` (name, arguments, minimum
role, summary). `register_commands` lets a backend with typed commands (Discord)
generate typed subcommands from the specs. Backends without them (Slack, Teams)
register a single `/ariel` command, and the core parses its text against the same
specs. Either way the router receives the same `Command`:

```rust
pub struct Command {
    pub name: String,
    pub args: Args,
    pub user: UserRef,
    pub at: Destination,
    pub responder: Box<dyn Responder>,
}

#[async_trait]
pub trait Responder: Send + Sync {
    async fn defer(&self, visibility: Visibility) -> Result<(), ProviderError>; // calling it twice does nothing extra
    async fn reply(&self, msg: &Message, visibility: Visibility) -> Result<MessageRef, ProviderError>;
}
```

- **Deadlines belong to the backend.** When a backend emits a `Command`, it
  starts a timer just short of its platform's deadline, and defers on the core's
  behalf if no reply or defer has happened. After a defer, `reply` uses the
  platform's follow-up path while it is valid (Discord's interaction token,
  Slack's `response_url`), then falls back to an ordinary send. Teams replies are
  always ordinary sends.
- **Private replies never become public.** `Visibility::Private` uses an
  ephemeral reply if `ephemeral_replies` is set, otherwise a direct message if
  `direct_messages` is set, and otherwise fails.
- **The router guarantees a reply.** It runs each command in its own task and
  answers a failed command with an error message, so backends need not detect a
  responder dropped without replying.

### Capabilities, fallbacks and errors

```rust
#[non_exhaustive]
pub struct Capabilities {
    pub edit: bool,
    pub direct_messages: bool,
    pub ephemeral_replies: bool,
    pub typed_commands: bool,
    pub commands_in_threads: bool,
    pub buttons: bool,
    pub threads: bool,
    pub reads_thread_replies: bool,
    pub limits: Limits,
}

pub struct Limits {
    pub title_chars: usize,
    pub body_chars: usize,
    pub fields: usize,
    pub field_chars: usize,
    pub actions: usize,
    pub total_chars: usize,
    pub send_budget: SendBudget,
}

/// A per-channel token bucket the core paces sends against, before any 429.
pub struct SendBudget { pub burst: u32, pub per_hour: u32 }
```

`send_budget` lets the core pace sends ahead of the platform instead of only
reacting to throttling: Discord advertises a burst of 5 and 3600 per hour, Slack
a burst of 3 and 3600 per hour (about one message a second per channel), and
Teams a burst of 7 and 1800 per hour (its per-conversation hourly cap).

The core applies fallbacks in one place before calling a provider, so a backend
never receives a message it cannot send:

- A body over the limit is truncated with a marker; the dashboard link carries
  the detail. Messages are never split across posts, which would spend rate-limit
  budget.
- Fields over the limit keep the first N−1 plus a "+k more" field.
- Actions are dropped when `buttons` is off; the link is kept.
- A new message is posted when `edit` is off.

```rust
#[non_exhaustive]
pub enum ProviderError {
    Unsupported(&'static str),
    RateLimited { retry_after: Option<Duration> },
    Forbidden,
    NotFound,
    InvalidMessage(String), // over a limit the core should have enforced: a bug
    Transport(Box<dyn std::error::Error + Send + Sync>),
}
```

A backend may absorb short, platform-managed waits (under about a second, such as
an SDK's own bucket handling). Longer waits surface as `RateLimited`, and queueing,
coalescing and dropping are the notification policy's concern (#4).

### Capability matrix

| Layer | Needed feature | Discord | Slack | Teams |
|---|---|---|---|---|
| 1 Notifications | Post to a channel | Yes | Yes | Yes, once installed; not private channels |
| | Edit in place | Yes | `chat.update` | `updateActivity` |
| | Tightest size | 6000 total, 4096 body | ~4000 text, 50 blocks | ~80 KB |
| | Posting rate | 50 req/s global, per-channel buckets | ~1 msg/s per channel | 7/s, 60/30 s, 1800/h per conversation |
| 2 Approvals | Buttons, update the clicked message | Components | Block Kit `block_actions` | Adaptive Card `Action.Execute` |
| 3 ChatOps | Typed commands | Yes | Free text | Free text |
| | Deadline, then follow-up path | 3 s, 15-minute token | 3 s, 5 uses in 30 min | 5 s, ordinary sends |
| | Private reply | Interaction replies only | Ephemeral, not guaranteed | Targeted, 24h expiry |
| | Commands in threads | Yes | No | Yes |
| | Receiving events | Gateway socket (outbound) | Socket Mode (outbound) | Public HTTPS only |
| 4 Conversation | Threads | Thread channels, auto-archive | Replies to a parent | Reply chains |
| | Reading replies | Privileged `MESSAGE_CONTENT` intent | History scopes, channel membership | Resource-specific consent |

### MVP boundary

- **In the MVP:** everything above for layers 1 and 3 — `id`, `capabilities`,
  `register_commands`, `post`, `inbound` yielding commands, `edit`,
  `direct_message`, `Responder`, `Capabilities` with `Limits`, and
  `ProviderError`. Only the Discord backend is built.
- **Designed, deferred:** `Inbound::Action` and message actions (#6),
  `start_thread` and `Inbound::ThreadReply` (#7), and the Slack and Teams
  backends. `Inbound` and `Capabilities` are `#[non_exhaustive]`, so adding them
  does not break existing backends.
- **Out of scope:** modals, reactions, file uploads, Discord autocomplete, and
  platform streaming APIs; editing in place covers progress updates.

### Testing

- **`ConsoleProvider`** (#11) is an in-memory provider in `ariel-core`. It
  records every outbound call, lets tests inject inbound events and inspect
  responder calls, and takes configurable capabilities, so each fallback is
  exercised.
- **A shared contract suite**, behind a `contract-tests` feature in `ariel-core`
  and modelled on gonzalo's conformance suite, runs against any provider plus a
  harness that fakes the platform side. It checks that `Unsupported` matches the
  flags, that a posted message can be edited, that a message at the advertised
  limits posts, that typed and parsed commands produce the same `Command`, that
  auto-defer fires before the deadline, and that a private reply never reaches a
  public destination. It runs against `ConsoleProvider` and against each backend
  with a stub HTTP server and recorded payloads, never a live platform.
- **Table-driven core tests** cover the fallback and truncation step, and
  **golden tests** cover free-text command parsing against the specs.

## Consequences

- **Positive:** The core, router and renderer are written once against one
  message model and one command model, and every fallback is decided and tested
  in one place. Backends stay thin: mapping, transport and deadlines. Deferred
  layers and new platforms are additive. The capability flags make platform
  gaps explicit and testable instead of hidden in backend code.
- **Negative:** A neutral message model gives up some platform-native polish.
  Backends carry real work the trait cannot see: auto-defer timers, follow-up
  expiry, and mapping one `Message` to three formats. The matrix records platform
  behaviour as of 2026-09-13 and will drift. Teams needs public ingress and has
  no Rust SDK, so its backend means hand-written Bot Framework REST and token
  validation. Slack's missing in-thread commands will need a mention fallback
  when threads arrive.
- **Revisit if:** a backend needs to branch on the provider in core code to work
  around a platform, a capability is needed that a flag cannot express, a deferred
  layer (#6, #7) cannot fit the variants reserved for it, or a platform changes
  its deadline or private-reply model enough to break the responder contract.
