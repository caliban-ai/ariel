# Architecture & Couplings

Ariel is a standalone service, a sibling to prospero and gonzalo, so that chat
platform SDKs, credentials, and rate limiting stay behind one boundary
([ADR 0002](./adr/0002-standalone-service-and-couplings.md)).

## The target shape

The diagram shows the design. Parts marked *planned* do not exist in code yet;
see [Status & Roadmap](./status.md).

```text
Discord / Slack / Teams
        │  Gateway socket (Discord); Slack and Teams planned
        ▼
┌──────────────────────────────────────────────────────────┐
│ arield                                                   │
│  ChatProvider trait · renderer · prospero client ·       │
│  fleet watcher                                           │
│  planned: notifier · router · auth · gonzalo client ·    │
│           session bridge                                 │
└──────────┬───────────────────────────────┬───────────────┘
           │ HTTP + SSE                    │ records (planned)
           ▼                               ▼
       prosperod                        gonzalod
  fleet snapshot, per-agent       people, roles, channel
  streams, spawn, kill, input     config, audit
```

## Couplings

- **Through prospero.** All fleet control and fleet events go over prosperod's
  public HTTP and SSE API. Ariel takes no dependency on prospero crates: it
  mirrors the wire types it reads in `ariel_core::prospero::types`, and golden
  fixtures written from prospero v0.7.0 pin them
  ([ADR 0005](./adr/0005-mirror-prospero-wire-types.md)). Every enum Ariel matches
  on has an `Unknown` fallback, so a newer prosperod does not break decoding.
- **With gonzalo.** Ariel stores nothing of its own. Identity, role grants,
  channel configuration, link tokens, and the audit trail are to be gonzalo
  records ([ADR 0003](./adr/0003-no-state-of-its-own.md)). The gonzalo client is
  not written yet, and the record kinds are still being designed upstream
  ([caliban-ai/gonzalo#277](https://github.com/caliban-ai/gonzalo/issues/277),
  [#278](https://github.com/caliban-ai/gonzalo/issues/278)).
- **To caliban only through prospero.** Ariel never talks to caliband.

## Talking to prosperod

`ProsperoClient` (in `ariel-core`) maps prosperod's routes one to one over plain
HTTP (TLS is not enabled in the build):

| Method | Route | Client call |
|---|---|---|
| `GET` | `/api/fleet` | `fleet()` |
| `POST` | `/api/workspaces/{workspace}/agents` | `spawn()` |
| `POST` | `/api/agents/{id}/kill` | `kill()` |
| `POST` | `/api/agents/{id}/respawn` | `respawn()` |
| `POST` | `/api/agents/{id}/input` | `input()` |
| `POST` | `/api/agents/{id}/end-input` | `end_input()` |
| `GET` | `/api/agents/{id}/stream?from={seq}` | `stream()` (SSE) |

A base URL with a path prefix, as behind a reverse proxy, is kept. Requests time
out after 30 seconds (the event stream excepted), and connecting after 5.

Prospero has no fleet-wide event stream, only one per agent. `FleetWatcher`
builds one: it polls `GET /api/fleet` (every 5 s by default), opens one SSE stream
per agent, and merges them into a single channel. Each agent's events arrive in
order and exactly once across reconnects, resuming from the last delivered `seq`.
Agents already terminal on the first poll are treated as history and not replayed.
Because prosperod only closes a stream after `agent_finished`, the watcher stops
listening to a killed or crashed agent after a linger (10 s by default).

## Crates

| Crate | Binary | Role |
|---|---|---|
| `ariel-core` | — | Provider-neutral core: the `ChatProvider` trait and `ConsoleProvider`, the prospero client and fleet watcher, and the renderer. Never depends on a chat platform SDK; CI checks this. |
| `ariel-discord` | — | Discord backend on twilight ([ADR 0010](./adr/0010-discord-library-twilight.md)). |
| `ariel-daemon` | `arield` | The long-running bridge. Today: configuration from files and the health endpoint. |
| `ariel-cli` | `ariel` | The operator CLI. Today it only reports its version. |

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
configuration is meant to decide which one runs. `arield` prints the providers
compiled into it at startup. `cargo build -p ariel-daemon --no-default-features`
builds with no chat backend, and CI keeps that build green.

## Rendering and notifications

Ariel's notification design is one live message per agent, edited in place, with
bursts of five or more spawns collapsed into one summary message, and sends paced
per channel in the core ([ADR 0007](./adr/0007-notifications-live-messages-and-pacing.md)).

The rendering half exists: `AgentView` folds `agent_spawned`, `status_changed`,
`agent_finished`, and `agent_gone` events into an agent's current state and reports
whether its message changed; `render_agent` and `render_summary` turn that state
into provider-neutral messages with a severity, fields for start and end time,
outcome, cost and turns, and an optional dashboard link. The notifier that holds
back, coalesces, and paces those messages is planned.

## Security and deployment

[ADR 0008](./adr/0008-secrets-deployment-and-network-boundary.md) sets the
deployment model: credentials only as mounted Secret files, a single-replica
Deployment with no ingress (Discord events arrive over an outbound Gateway
connection), a read-only root filesystem, and a NetworkPolicy fencing the
unauthenticated prosperod so Ariel is the gated path to it. The file-based
credentials, health endpoint and image are implemented; the Helm chart and
cluster rollout live in other repositories.
