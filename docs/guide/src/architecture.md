# Architecture & Couplings

Ariel is a standalone service, a sibling to prospero and gonzalo, so that chat
platform SDKs, credentials, and rate limiting stay behind one boundary
([ADR 0002](./adr/0002-standalone-service-and-couplings.md)).

## The shape

Everything in the diagram exists in code. The session bridge that would make a
chat thread an agent session does not, and neither do the Slack and Teams
backends; see [Status & Roadmap](./status.md).

```text
Discord                                   (Slack / Teams: not written)
   │  Gateway socket (interactions in), REST (messages out)
   ▼
┌──────────────────────────────────────────────────────────┐
│ arield                                                   │
│  ChatProvider trait · notifier · command router ·        │
│  two-key authorization · renderer · prospero client ·    │
│  fleet watcher · gonzalo records client                  │
└──────────┬───────────────────────────────┬───────────────┘
           │ HTTP + SSE                    │ records
           ▼                               ▼
       prosperod                        gonzalod
  fleet snapshot, per-agent       people, bindings, roles,
  streams, spawn, kill, input     channel config, link
                                  tokens, audit
```

## Couplings

- **Through prospero.** All fleet control and fleet events go over prosperod's
  public HTTP and SSE API. Ariel takes no dependency on prospero crates: it
  mirrors the wire types it reads in `ariel_core::prospero::types`, and golden
  fixtures written from prospero v0.8.1 pin them, with v0.9's `on_behalf_of` on
  the event envelope pinned by its own wire tests
  ([ADR 0005](./adr/0005-mirror-prospero-wire-types.md)). Every enum Ariel matches
  on has an `Unknown` fallback, so a newer prosperod does not break decoding, and
  a field a newer prosperod adds is optional, so an older one still decodes.
  Ariel authenticates with a bearer token from `ARIEL_PROSPERO_TOKEN_FILE`, which
  prosperod requires from v0.8 with API authentication on
  ([ADR 0013](./adr/0013-ariel-authenticates-to-prosperod.md)).
- **With gonzalo.** Ariel stores nothing of its own. Identity, role grants,
  channel configuration, link tokens, and the audit trail are gonzalo records
  ([ADR 0003](./adr/0003-no-state-of-its-own.md)), read and written through
  `ariel_core::records`. The record kinds are gonzalo's own, defined by its
  ADR 0022 and ADR 0023 and shipped from **gonzalo 0.7.0**
  ([gonzalo compatibility](./configuration.md#gonzalo-compatibility)).
- **To caliban only through prospero.** Ariel never talks to caliband.

## Talking to prosperod

`ProsperoClient` (in `ariel-core`) maps prosperod's routes one to one:

| Method | Route | Client call |
|---|---|---|
| `GET` | `/api/session` | `session()` — who prosperod thinks Ariel is, and with which scopes |
| `GET` | `/api/fleet` | `fleet()` |
| `POST` | `/api/workspaces/{workspace}/agents` | `spawn()` |
| `POST` | `/api/agents/{id}/kill` | `kill()` |
| `POST` | `/api/agents/{id}/respawn` | `respawn()` |
| `POST` | `/api/agents/{id}/input` | `input()` |
| `POST` | `/api/agents/{id}/end-input` | `end_input()` |
| `GET` | `/api/agents/{id}/stream?from={seq}` | `stream()` (SSE) |

A base URL with a path prefix, as behind a reverse proxy, is kept. Requests time
out after 30 seconds (the event stream excepted), and connecting after 5. Both
`http` and `https` work: TLS goes through rustls and verifies against the
platform's trust store, which the runtime image populates. In-cluster traffic to
a Service stays plain `http`; `https` matters for reaching a prosperod through an
ingress. `arield` calls `session()` at startup whenever `ARIEL_PROSPERO_URL` is
set, so a refused or missing token is an error in the log rather than a surprise
at the first command.

`on_behalf_of(person)` returns a clone of the client that adds prospero v0.9's
`X-Prospero-On-Behalf-Of` header to every request it makes. Ariel holds one
`operate` token for a whole chat workspace, so the token alone cannot tell one
person's agents from another's; the person id travels beside it and comes back
on the events those requests emit. A value prosperod would answer `400` to is
dropped rather than failing the request it rode on — a gonzalo person id never
is one, but a rejected header would fail the whole command.

Prospero had no fleet-wide event stream when Ariel was written, only one per
agent, so `FleetWatcher` builds one: it polls `GET /api/fleet` (every 5 s by
default), opens one SSE stream per agent, and merges them into a single channel.
Each agent's events arrive in order and exactly once across reconnects, resuming
from the last delivered `seq`. Agents already terminal on the first poll are
treated as history and not replayed. Because prosperod only closes a stream after
`agent_finished`, the watcher stops listening to a killed or crashed agent after
a linger (10 s by default).

Prospero v0.9.0 ships `GET /api/fleet/stream`, which carries every stream's
events on one connection with a fleet-wide cursor as each event's SSE `id:`.
Replacing the watcher with it is [#55](https://github.com/caliban-ai/ariel/issues/55),
and is no longer blocked on prospero.

## Crates

| Crate | Binary | Role |
|---|---|---|
| `ariel-core` | — | Provider-neutral core: the `ChatProvider` trait and `ConsoleProvider`, the prospero client and fleet watcher, the renderer, the notifier, the command handlers, two-key authorization, account linking, channel parsing, and the gonzalo records client. Never depends on a chat platform SDK; CI checks this. |
| `ariel-discord` | — | Discord backend on twilight ([ADR 0010](./adr/0010-discord-library-twilight.md)). |
| `ariel-daemon` | `arield` | The long-running bridge: configuration, logging, the health endpoint, and the wiring that joins the fleet watcher, the per-channel notifiers and the command router. |
| `ariel-cli` | `ariel` | The operator CLI: `ariel channel show`/`set` and `ariel link new` ([The `ariel` CLI](./cli.md)). |
| `ariel-e2e` | — | Nothing ships from it: it holds the headless end-to-end smoke test, which runs the bridge against prosperod's and gonzalod's own server code. Kept apart so prospero and gonzalo-server stay out of every other crate's dependency tree, and excluded from the main CI gate, which runs it in its own job. |

`ariel-core`'s modules map onto those jobs: `chat` (the provider boundary),
`prospero`, `records` (gonzalo), `render`, `notify`, `commands`, `auth`, `link`
and `channels`.

## Chat providers and features

Every platform sits behind one `ChatProvider` trait
([ADR 0004](./adr/0004-provider-trait-and-feature-gated-backends.md),
[ADR 0006](./adr/0006-chat-provider-trait.md)): a small required core (`id`,
`capabilities`, `register_commands`, `post`, `inbound`) plus capability-gated
methods (`edit`, `direct_message`, `start_thread`) that default to
`Unsupported`. A `Capabilities` descriptor, including message size limits and a
per-channel send budget, tells the core which it may call.

Each backend is its own crate, consumed by `ariel-daemon` as an optional
dependency behind a feature of the same name:

| Crate | Feature | Default | Notes |
|---|---|---|---|
| `ariel-daemon` | `discord` | yes | Compiles in `ariel-discord`. |
| `ariel-core` | `contract-tests` | no | Exposes the shared provider contract suite for backends' tests. |

Features are additive: they decide which providers *can* exist in a build, and
configuration decides which one runs — today, whether the Discord token, guild ID
and application ID are all set. `arield` logs the providers compiled into it at
startup, on the `arield starting` line.
`cargo build -p ariel-daemon --no-default-features` builds with no chat backend,
and CI keeps that build green.

## Rendering and notifications

Ariel's notification design is one live message per agent, edited in place, with
bursts of five or more spawns collapsed into one summary message, and sends paced
per channel in the core ([ADR 0007](./adr/0007-notifications-live-messages-and-pacing.md)).
Both halves exist.

`AgentView` folds `agent_spawned`, `status_changed`, `agent_finished`, and
`agent_gone` events into an agent's current state and reports whether its message
changed; `render_agent` and `render_summary` turn that state into
provider-neutral messages with a severity, fields for the start time, who
started it, the end time, outcome, cost and turns, and an optional dashboard
link. The attribution belongs to the spawn: a later event carrying someone
else — a kill, say — does not rewrite who started the agent.

`Notifier` then holds one per channel. It decides for itself whether it follows
an event's workspace and whether the channel's notify preset wants that event
kind ([ADR 0009](./adr/0009-channel-config.md)), keeps the live message's id so a
change is an edit rather than a new post, collapses a burst, and paces its sends
against the provider's advertised `SendBudget`. A rate limit, a lost permission
or a message somebody deleted is handled rather than fatal. `arield` starts one
notifier per configured channel and feeds every fleet event to all of them, so
adding a channel is a record, not a code change.

What arrives on an event is the gonzalo **person id** Ariel asserted, not a
display name: ids are stable and unique, names change and collide. So the
notifier resolves it before the channel sees it, reading that person's record
once and remembering the answer. A failed or unknown lookup is not cached — the
person may be created later — and the id is shown as it came rather than
dropped, which still tells two people's agents apart.

## Commands and authorization

A command arrives as an `Inbound::Command` from the provider and is answered by
`ariel_core::commands`. Two keys decide what it may do: the role the person
holds, from their gonzalo grants, and the ceiling on the channel's configuration
record. The command runs at the **lower** of the two
([ADR 0009](./adr/0009-channel-config.md),
[ADR 0012](./adr/0012-command-authorization-and-audit.md)), and every command
that changes something leaves exactly one audit entry in gonzalo, `Denied` or
`Succeeded`/`Failed`. An unlinked chat account gets nothing but instructions for
linking, and an unconfigured channel is refused before prosperod is asked
anything, so it cannot be used to discover which agents exist. The details, and
the table of who may run what, are in [Chat Commands](./commands.md).

An allowed decision carries the person it is for, and `spawn`, `kill` and
`respawn` run against a client clone that names them to prosperod, so the fleet
events a command causes are attributed to the person rather than only to Ariel's
token ([Who started an agent](./commands.md#who-started-an-agent)). Prosperod
does not verify the claim — it cannot authenticate someone else's user — so the
token stays the authenticated identity and a false claim stays attributable to
the credential that made it.

`/ariel link` is the one command that works in any channel: it redeems a
one-time token minted by `ariel link new` or `/ariel invite`, binds the chat
account to a person and grants the token's role. Only a hash of the token is
stored, and it is marked used rather than deleted, so a sync between gonzalo
stores cannot bring it back.

## Security and deployment

[ADR 0008](./adr/0008-secrets-deployment-and-network-boundary.md) sets the
deployment model: credentials only as mounted Secret files, a single-replica
Deployment with no ingress (Discord events arrive over an outbound Gateway
connection), and a read-only root filesystem.
[ADR 0013](./adr/0013-ariel-authenticates-to-prosperod.md) revisits its
assumption that prosperod is unauthenticated: from prospero v0.8 Ariel presents
a scoped API token, so the NetworkPolicy fencing prosperod is defence in depth
rather than the only control.

The file-based credentials, health endpoint, logging and image are implemented.
The cluster rollout is the `ariel` chart in
[caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts), which tracks
the published image.
