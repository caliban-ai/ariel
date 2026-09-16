# ADR 0009 · Channel configuration: what a channel follows, hears, and allows

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` (caliban-ai umbrella workspace) §Identity, RBAC & audit (`ChannelConfig`), open question 4; issue #3

## Context

Each chat channel Ariel serves has a configuration record in gonzalo
([ADR 0003](0003-no-state-of-its-own.md)). The design spec gave it a role ceiling,
the workspaces it follows, and filters, but left open how many workspaces a
channel can follow, what filters may change, the defaults for a new channel, and
how filters relate to the ceiling. Prospero has since renamed repositories to
workspaces, so the record uses workspaces throughout.

Two decisions already constrain it:

- [ADR 0006](0006-chat-provider-trait.md) keys every channel by provider, tenant
  and channel ID.
- [ADR 0007](0007-notifications-live-messages-and-pacing.md) fixes which event
  kinds notify and how they are paced: one live message per agent, burst
  summaries, and no `Output` or tool events.

The record's shape must be settled before gonzalo defines the access-control
record kinds (caliban-ai/gonzalo#277).

Alternatives weighed:

- **Cardinality:** exactly one workspace per channel (an ops view of the fleet
  would then need a channel per workspace), or every channel receiving the whole
  fleet narrowed by filters (a missing filter floods a team channel).
- **Filters:** per-kind include and exclude lists, which would let a channel opt
  into `Output` and break ADR 0007's pacing; or no filters, which leaves a busy
  fleet channel unable to hear only about failures.
- **Defaults:** following the whole fleet at `operator`, which makes a new channel
  noisy and privileged; or the whole fleet at failures-only, which needs two
  changes before a team channel is useful.

## Decision

Each configured channel has **one `ChannelConfig` record**:

```rust
pub struct ChannelConfig {
    pub channel: ChannelRef,   // key: provider, tenant, channel (ADR 0006)
    pub follows: Follows,      // required; no default
    pub notify: NotifyPreset,  // default All
    pub ceiling: Role,         // default Viewer
}

pub enum Follows { Fleet, Workspaces(BTreeSet<String>) } // the set is non-empty
pub enum NotifyPreset { All, Terminal, Failures }
pub enum Role { Viewer, Operator, Admin }                // Viewer < Operator < Admin
```

### What a channel follows

- `Fleet` follows every workspace, including ones created later, so it never needs
  updating when a workspace is added.
- `Workspaces` follows a non-empty set of workspace names, as prospero reports
  them. Following a name prospero does not list yet is allowed, since the
  workspace may be created later, but `ariel channel show` warns about it.

### What a channel hears

An event notifies a channel only if its workspace is followed **and** the channel's
preset admits it. Presets can only narrow ADR 0007's defaults, never add event kinds:

- **`All`**: ADR 0007's live message per agent, with burst summaries.
- **`Terminal`**: no live message; one post when an agent ends.
- **`Failures`**: a post only when an agent ends failed or crashed, or is gone.
  ADR 0007's burst rule applies to these posts too: 5 or more within the window
  share one summary, so a wave of crashes cannot flood the channel.

### What a channel allows

- **The ceiling governs commands only.** It never hides notifications. A command's
  effective role is the lower of the person's role and the channel's ceiling.
- **Commands act only on followed workspaces.** A channel following `[caliban]`
  cannot spawn or kill agents in another workspace; a `Fleet` channel can act on
  any workspace.
- **An unconfigured channel** receives no notifications, and commands there are
  rejected with a hint, except `/ariel link`, which replies privately.
- **Changing a channel's configuration requires `admin`**, and every change is
  audited.

### Defaults

`ariel channel add` requires `--follows`, so no channel silently receives the
whole fleet. `notify` defaults to `All` and `ceiling` to `Viewer`: anyone linked
can use read-only commands, and spawning or killing needs the ceiling raised on
purpose.

### Worked examples

| Channel | `follows` | `notify` | `ceiling` | Effect |
|---|---|---|---|---|
| `#ops` | `Fleet` | `Failures` | `Viewer` | Hears only about failures across the whole fleet, including workspaces added later. Only read-only commands such as `/ariel status` work, even for admins. |
| `#caliban-team` | `Workspaces {caliban}` | `All` | `Operator` | A live message per `caliban` agent. Linked operators and admins can spawn and kill agents, but only in `caliban`. |

### Fields for caliban-ai/gonzalo#277

The `ChannelConfig` record kind needs exactly these fields:

| Field | Type | Required | Default |
|---|---|---|---|
| `provider` | string, e.g. `discord` | yes | — |
| `tenant` | string: guild, workspace or tenant ID | yes | — |
| `channel` | string: platform channel ID | yes | — |
| `follows` | tagged: `{ "kind": "fleet" }` or `{ "kind": "workspaces", "names": [...] }` with at least one name | yes | — |
| `notify` | `all`, `terminal` or `failures` | no | `all` |
| `ceiling` | `viewer`, `operator` or `admin` | no | `viewer` |

The record is keyed by `provider`, `tenant` and `channel`. It belongs to no person.
Who changed it is recorded in the audit trail and in gonzalo's author stamp, not in
the record.

### How gonzalo implemented it

gonzalo adopted these fields in
[ADR 0023](https://github.com/caliban-ai/gonzalo/blob/main/docs/adr/0023-channel-config-fields.md)
(caliban-ai/gonzalo#297), keyed `fleet/channels/<provider>:<tenant>:<channel>`.
Two details of the stored form differ from the table above, which described the
logical fields rather than a serialization Ariel owns:

- `follows` is stored inside gonzalo's atomic wrapper, as a one-element array:
  `"follows":[{"kind":"workspaces","names":["caliban"]}]`. That wrapper is what
  makes two concurrent follow edits conflict instead of merging field by field,
  which would otherwise pair one writer's `kind` with another's `names`.
- `ceiling` serializes capitalized (`"Viewer"`), because `FleetRole` is shared
  with role grants and link tokens. Ariel reads it as a typed value, so the
  spelling does not reach Ariel's own behaviour.

gonzalo also enforces the non-empty rule on decode, so a hand-written record
following an empty set of workspaces is rejected, not silently accepted.

## Consequences

- **Positive:** Both required examples are one record each. No channel receives
  the whole fleet by accident, and no new channel grants more than read-only
  commands. ADR 0007's pacing holds for every channel, because no configuration can
  add noisy event kinds. Scoping commands to followed workspaces keeps a team's
  channel from acting on another team's work.
- **Negative:** A channel cannot ask for a specific mix of event kinds or
  transitions outside the three presets. A `Fleet` channel with a high ceiling can
  act on every workspace, so its ceiling needs care. Following a name that is
  misspelled produces only a warning, not an error.
- **Revisit if:** users need a combination the presets cannot express, per-workspace
  presets within one channel, or thread channels (#7) need their own configuration.
