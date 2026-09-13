# ADR 0007 · Notifications: one live message per agent, paced per channel

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` (caliban-ai umbrella workspace) open question 7; issue #4

## Context

Prospero emits an `Output` event for every streamed chunk and a
`ToolStarted`/`ToolFinished` pair for every tool call, so a busy agent produces
far more events than a chat channel can absorb. Chat platforms throttle hard:
Slack allows about one message a second per channel, Teams caps a bot at 7 sends
a second and 1800 an hour per conversation, and Discord applies per-channel
buckets under a 50 requests a second global limit. Posting one message per event
would hit those limits within seconds and bury the channel.

[ADR 0006](0006-chat-provider-trait.md) keeps backends thin: they absorb only
short, platform-managed waits and surface longer ones as
`ProviderError::RateLimited`. It also gives every backend `edit` (where
supported) and a per-channel `Limits.send_budget`. Queueing, coalescing and
dropping were left to this decision.

The alternatives weighed for how notifications appear:

- **One post per notable event**, coalescing bursts into a digest post. The
  history is readable, but a busy fleet fills the channel.
- **A periodic digest**, with immediate posts only for failures. The channel is
  quietest, but nothing except failures is real time.
- **One live message per agent, edited in place.** A burst costs edits, not new
  posts.

And for where pacing lives: in each backend, which writes the burst logic three
times against ADR 0006's thin-backend rule, or in the core.

## Decision

We will notify with **one live status message per agent, edited in place, paced
and coalescing in the core.**

### Pipeline

Events flow from the `FleetWatcher`, through the channel's filters (#3), to a
`Notifier` in `ariel-core`, one per configured channel, which calls the
`ChatProvider`.

- **Notified kinds.** `AgentSpawned` creates an agent's live message.
  `StatusChanged` updates it. `AgentFinished` (outcome, cost, turns) and
  `AgentGone` finalize it. `Output`, `ToolStarted`, `ToolFinished`, `AgentInit`,
  `StorePersistFailed`, `RepoHealth` and unknown kinds do not notify by default.
- **Latest state wins.** For each agent the notifier keeps the posted
  `MessageRef`, the latest rendered state, and whether an edit is pending. Any
  number of changes before the next edit slot produce one edit.
- **Nothing terminal is lost.** Intermediate states are superseded, never queued,
  so pending work is bounded by the number of live agents, not the number of
  events.
- **Restarts.** Message refs are not persisted ([ADR 0003](0003-no-state-of-its-own.md)).
  After a restart a running agent gets a fresh live message; the old one is left
  as it was.

### Bursts

- A new agent's first post waits **2 seconds**, so the notifier can tell a lone
  agent from a burst. First notifications are therefore delayed by up to 2 s.
- When **5 or more** agents spawn in the same channel and workspace within that
  hold, they share **one summary message**, edited in place with counts of done,
  running and failed. Any spawn within **10 seconds** of the summary's latest
  member joins it.
- Agents that end **failed or crashed** still get their own post, so a failure is
  never hidden inside a count. Killed agents stay in the summary, since killing
  is deliberate.
- The hold, threshold and window are configurable defaults.

### Pacing and failures

- **One sender per channel, highest priority first:** terminal updates (final
  edits and failure posts), then new live messages and summaries, then
  intermediate edits.
- **Pace ahead of the platform.** The sender runs `Limits.send_budget` as a token
  bucket (Discord burst 5 and 3600 an hour, Slack burst 3 and 3600 an hour, Teams
  burst 7 and 1800 an hour), and spaces edits to the same message at least
  **2 seconds** apart.
- **`RateLimited { retry_after }`** pauses that channel's sender for `retry_after`,
  or backs off exponentially from 1 s to 60 s with jitter when none is given.
  Work keeps collapsing during the pause, and new spawns count toward a summary.
  After the pause the current state is rendered again; a stale edit is never
  resent.
- **Transport errors** back off the same way.
- **`NotFound`** means the live message was deleted: its ref is dropped and the
  next update posts one new message.
- **`Forbidden`** means the bot lost access: the channel is marked unhealthy, the
  failure is logged, and nothing is sent to it until a re-check every
  5 minutes.
- **After a long outage**, terminal states are sent first, then each running
  agent's current state. Missed intermediate states are not replayed.
- **Without edit support**, an agent gets one post at spawn (or a place in a
  summary) and one terminal post.

### Behaviour under test

Each scenario runs against `ConsoleProvider` with paused tokio time, so the
expected numbers are exact.

| Scenario | Input | Expected |
|---|---|---|
| A. One chatty agent | Spawn at 0 s, 20 status changes within 3 s, finish at 10 s | 1 post; edits at least 2 s apart; the final message shows outcome, cost and turns |
| B. Burst | 30 spawns in one workspace within 2 s; 2 crash at 30 s; the rest finish by 120 s | 1 summary post at about 2 s; 2 failure posts at about 30 s; no individual posts for the other 28; the final summary reads 28 done and 2 failed; summary edits at least 2 s apart |
| C. Rate limited | The next send returns `RateLimited { retry_after: 5s }`; 10 status changes and one finish arrive during the pause | After 5 s, the terminal update first, then exactly one edit with the latest state for each other agent |
| D. Budget | Teams budget (burst 7, 1800 an hour); 3000 status changes over an hour | Never more than 7 sends in a burst or 1800 in the hour; every terminal state delivered |
| E. Message deleted | An edit returns `NotFound` | The next update posts exactly one new message |
| F. Bot removed | A send returns `Forbidden` | No further sends until the 5-minute re-check |

## Consequences

- **Positive:** A busy fleet costs a channel one message per agent, or one per
  burst, regardless of how many events prospero emits. Pacing, coalescing and
  failure handling are written and tested once, in the core, for every platform.
  Failures always stand out. Pending work is bounded by live agents, so an outage
  cannot grow an unbounded queue.
- **Negative:** First notifications are delayed by up to 2 seconds. Channel
  history shows only each agent's latest state, not its path there. Live messages
  from before a restart go stale and are not cleaned up. The send budgets are
  hand-set from platform documentation and can drift from real limits, in which
  case the 429 backoff is the safety net.
- **Revisit if:** channels need a record of intermediate states, the 2-second
  hold is noticeable to users, restarts leave enough stale messages to confuse
  people (persisting refs in gonzalo would then be worth it), or observed 429s
  show a budget is wrong.
