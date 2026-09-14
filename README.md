# Ariel

[![license](https://img.shields.io/badge/license-AGPL--3.0--only-blue)](LICENSE)
[![guide](https://img.shields.io/badge/guide-caliban--ai.github.io%2Fariel-informational)](https://caliban-ai.github.io/ariel/)

Ariel is the **chat bridge** for the [Caliban](https://github.com/caliban-ai/caliban)
agent fleet. It brings fleet notifications, commands, approvals, and conversation
into the chat platforms where teams already work: Discord first, then Slack and
Microsoft Teams.

In *The Tempest*, Ariel is the spirit Prospero sends to carry messages. Here it
carries them between people in chat and the fleet that
[Prospero](https://github.com/caliban-ai/prospero) runs.

> **Status: foundation built, not yet wired.** The provider trait, prospero
> client, renderer and Discord backend exist and are tested, but `arield` does
> not yet connect them into a running bridge, and no chat commands exist. The
> identity layer is blocked on upstream work in gonzalo
> ([#277](https://github.com/caliban-ai/gonzalo/issues/277),
> [#278](https://github.com/caliban-ai/gonzalo/issues/278)). No release has been
> tagged. Details: [Status & Roadmap](https://caliban-ai.github.io/ariel/status.html).

## What works today

- **`ariel-core`**: the `ChatProvider` trait, an in-memory `ConsoleProvider`, and a
  shared contract suite (`contract-tests` feature) every backend runs; a typed
  client for prosperod's HTTP and SSE API (fleet, spawn, kill, respawn, input,
  stream); a `FleetWatcher` that merges prospero's per-agent streams into one
  fleet-wide feed; and a renderer that turns an agent's state into a chat message.
- **`ariel-discord`**: a Discord backend on twilight that registers `/ariel` slash
  commands, receives them over the Gateway, answers within Discord's deadline
  (auto-deferring), and posts, edits, direct-messages and starts threads. It runs
  against a real guild through a [manual smoke test](docs/discord-smoke-test.md).
- **`arield`**: reads credentials from files, reports the chat providers compiled
  in, serves `GET /healthz`, and stops cleanly on SIGTERM.
- **`ariel`** (CLI): version only.
- **Container**: a `Dockerfile` and a release workflow for multi-arch
  `ghcr.io/caliban-ai/ariel` images on `v*` tags.

Next is the MVP walking skeleton ([#22](https://github.com/caliban-ai/ariel/issues/22)):
daemon wiring and notifications, the gonzalo client, account linking, two-key
authorization, and `/ariel status` and `/ariel spawn`.

## How it works

Ariel is a standalone service, a sibling to prospero and gonzalo, so that chat
platform SDKs, credentials, and rate limiting stay behind one boundary. Parts
marked *planned* are designed but not yet in code.

```
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

- **Through prospero.** All fleet control and events go over prospero's public
  HTTP and SSE API. Ariel does not depend on prospero crates; it mirrors the wire
  types it reads, pinned by golden fixtures.
- **With gonzalo.** Identity, role grants, channel configuration, and the audit
  trail are to be gonzalo records. Ariel stores nothing of its own.
- **To caliban** only indirectly, through prospero.

Chat platforms sit behind one `ChatProvider` trait, and each backend is a
feature-gated crate. A build without the `discord` feature cannot load Discord.

## Capabilities

One bridge at four depths, each shippable on its own:

| Layer | Direction | What it does | State |
|---|---|---|---|
| Notifications | out | Agent started, changed status, finished | Watcher and renderer built; paced notifier and wiring next |
| ChatOps | both | Slash commands to list, spawn, kill, and restart agents | Discord command plumbing built; no commands yet |
| Approvals | both, narrow | Approve or deny a risky action with buttons | Deferred; blocked on upstream caliban and prospero work |
| Conversational | both, full | A chat thread is an agent session | Deferred |

Commands are to be authorized on two keys: a person's role and a ceiling set on
the channel. The lower of the two wins.

## Quickstart

```sh
cargo build --workspace
cargo test --workspace

cargo run -p ariel-daemon --bin arield     # prints compiled providers, serves :8081
curl http://127.0.0.1:8081/healthz         # ok
```

Container:

```sh
docker build -t ariel:dev .
docker run --rm --read-only -p 8081:8081 ariel:dev
```

## Configuration

`arield` reads only environment variables:

| Variable | Default | Meaning |
|---|---|---|
| `ARIEL_HEALTH_ADDR` | `0.0.0.0:8081` | Where `/healthz` is served |
| `ARIEL_DISCORD_TOKEN_FILE` | unset | File holding the Discord bot token |
| `ARIEL_GONZALO_TOKEN_FILE` | unset | File holding the gonzalod bearer token |

Credentials are always files, never plain variables. A named file that is
missing, unreadable or empty stops `arield` at startup. The token files are
validated but not yet used, since the daemon does not connect to Discord or
gonzalod yet. See the [configuration reference](https://caliban-ai.github.io/ariel/configuration.html).

## Documentation

- Guide: <https://caliban-ai.github.io/ariel/> (source in [`docs/guide/`](docs/guide/))
- Decisions: [`docs/adr/`](docs/adr/README.md), the architecture decision log.
- Discord: [`docs/discord-smoke-test.md`](docs/discord-smoke-test.md).
- Design spec: `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md`
  in the caliban-ai umbrella workspace.
- Tracking: [caliban-ai/prospero#67](https://github.com/caliban-ai/prospero/issues/67).

## License

[AGPL-3.0-only](LICENSE), matching the rest of the caliban-ai stack.
