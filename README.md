# Ariel

[![license](https://img.shields.io/badge/license-AGPL--3.0--only-blue)](LICENSE)
[![guide](https://img.shields.io/badge/guide-caliban--ai.github.io%2Fariel-informational)](https://caliban-ai.github.io/ariel/)
[![release](https://img.shields.io/badge/release-v0.3.0-blue)](https://github.com/caliban-ai/ariel/releases/tag/v0.3.0)

Ariel is the **chat bridge** for the [Caliban](https://github.com/caliban-ai/caliban)
agent fleet. It brings fleet notifications, commands, approvals, and conversation
into the chat platforms where teams already work: Discord first, then Slack and
Microsoft Teams.

In *The Tempest*, Ariel is the spirit Prospero sends to carry messages. Here it
carries them between people in chat and the fleet that
[Prospero](https://github.com/caliban-ai/prospero) runs.

> **Status: running bridge, Discord only.** `arield` watches prosperod's fleet,
> keeps one live message per agent in every channel configured to follow that
> agent's workspace, and answers eight `/ariel` commands — authorized on the
> person's role and the channel's ceiling, audited in gonzalo. The current
> release is **v0.3.0**, published as
> [`ghcr.io/caliban-ai/ariel:0.3.0`](https://github.com/caliban-ai/ariel/pkgs/container/ariel)
> for `linux/amd64` and `linux/arm64`. Approvals and conversational threads are
> designed and deferred; Slack and Teams are not written.
> Details: [Status & Roadmap](https://caliban-ai.github.io/ariel/status.html).

## What works today

- **Notifications.** `arield` watches the fleet and posts one live message per
  agent, edited in place as it changes, to every channel that follows its
  workspace. A burst of five or more spawns collapses into a summary, and sends
  are paced against the chat platform's own budget.
- **Attribution.** An agent started from chat says who asked for it: `spawn`,
  `kill` and `respawn` name the person to prosperod (v0.9's
  `X-Prospero-On-Behalf-Of`), and its live message carries a `started by` field
  resolved to that person's display name. An agent started outside Ariel, or
  against a prosperod older than v0.9, renders exactly as before.
- **Chat commands.** `/ariel link`, `/ariel status`, `/ariel spawn`,
  `/ariel kill`, `/ariel respawn`, `/ariel channel`, `/ariel configure` and
  `/ariel invite`
  ([Chat Commands](https://caliban-ai.github.io/ariel/commands.html)). Every
  command runs at the lower of the person's role and the channel's ceiling, and
  every command that changes something leaves one audit entry in gonzalo.
- **Identity.** `ariel link new` mints a one-time token; the person redeems it
  in chat with `/ariel link`, or an admin hands one out with `/ariel invite`.
  Only a hash of a token is stored, and it works once.
- **Channel configuration.** What a channel follows, how much it hears and its
  command ceiling is a gonzalo record, set from the CLI (`ariel channel set`) or
  from chat (`/ariel configure`). `arield` re-reads the records every
  `ARIEL_CHANNEL_RELOAD_SECS` (60 by default), so a channel added, retired or
  re-scoped needs no restart.
- **Operations.** Logs to stderr (`RUST_LOG`, `ARIEL_LOG_FORMAT=json`), serves
  `GET /healthz`, reads every credential from a mounted file, speaks `https` to
  prosperod and gonzalod, and shuts down cleanly on SIGTERM.
- **Released as a container.** `ghcr.io/caliban-ai/ariel`, multi-arch, tagged by
  version and `sha-<commit>` on every `v*` tag. A Helm chart lives in
  [caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts).

What is left is in [Status & Roadmap](https://caliban-ai.github.io/ariel/status.html):
the approvals layer ([#6](https://github.com/caliban-ai/ariel/issues/6)) and
conversational threads ([#7](https://github.com/caliban-ai/ariel/issues/7)) are
designed and deferred, and the Slack and Teams backends are not written.

## How it works

Ariel is a standalone service, a sibling to prospero and gonzalo, so that chat
platform SDKs, credentials, and rate limiting stay behind one boundary.

```
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

- **Through prospero.** All fleet control and events go over prospero's public
  HTTP and SSE API. Ariel does not depend on prospero crates; it mirrors the wire
  types it reads, pinned by golden fixtures from prospero v0.8.1.
- **With gonzalo.** Identity, role grants, channel configuration, link tokens and
  the audit trail are gonzalo records (gonzalo 0.7.0 or newer). Ariel stores
  nothing of its own.
- **To caliban** only indirectly, through prospero.

Chat platforms sit behind one `ChatProvider` trait, and each backend is a
feature-gated crate. A build without the `discord` feature cannot load Discord.

## Capabilities

One bridge at four depths, each shippable on its own:

| Layer | Direction | What it does | State |
|---|---|---|---|
| Notifications | out | Agent started, changed status, finished | **Built** — one live message per agent naming who started it, burst summaries, paced sends |
| ChatOps | both | Slash commands to list, spawn, kill, and restart agents | **Built** — eight `/ariel` commands, two-key authorized and audited |
| Approvals | both, narrow | Approve or deny a risky action with buttons | Designed, deferred ([#6](https://github.com/caliban-ai/ariel/issues/6)); needs a permission-request event from caliban and prospero |
| Conversational | both, full | A chat thread is an agent session | Designed, deferred ([#7](https://github.com/caliban-ai/ariel/issues/7)) |

Commands are authorized on two keys: a person's role and a ceiling set on the
channel. The lower of the two wins.

## Quickstart

From source:

```sh
cargo build --workspace --exclude ariel-e2e
cargo test --workspace --exclude ariel-e2e

cargo run -p ariel-daemon --bin arield     # serves :8081; health only until configured
curl http://127.0.0.1:8081/healthz         # ok
```

From the published image:

```sh
docker run --rm --read-only -p 8081:8081 \
  -v "$PWD/secrets:/run/secrets/ariel:ro" \
  -e ARIEL_PROSPERO_URL=http://prosperod:7878 \
  -e ARIEL_GONZALO_URL=http://gonzalod:8080 \
  -e ARIEL_DISCORD_TOKEN_FILE=/run/secrets/ariel/discord-token \
  -e ARIEL_GONZALO_TOKEN_FILE=/run/secrets/ariel/gonzalo-token \
  -e ARIEL_DISCORD_GUILD_ID=123456789 \
  -e ARIEL_DISCORD_APPLICATION_ID=987654321 \
  ghcr.io/caliban-ai/ariel:0.3.0
```

A bare daemon notifies nothing until a channel has a configuration record and
your chat account is linked:

```sh
export ARIEL_GONZALO_URL=http://127.0.0.1:8080
export ARIEL_GONZALO_TOKEN_FILE=./secrets/gonzalo-token

ariel channel set --provider discord --tenant 123456789 --channel 987654321 \
  --follows fleet --notify all --ceiling operator
ariel link new --role operator          # redeem in chat with /ariel link
```

Full walkthrough: [Getting Started](https://caliban-ai.github.io/ariel/getting-started.html).

## Configuration

`arield` reads only environment variables; credentials are always files
([full reference](https://caliban-ai.github.io/ariel/configuration.html)):

| Variable | Default | Meaning |
|---|---|---|
| `ARIEL_PROSPERO_URL` | unset | prosperod's base URL (`http` or `https`) |
| `ARIEL_GONZALO_URL` | unset | gonzalod's base URL, where the records live |
| `ARIEL_DISCORD_TOKEN_FILE` | unset | File holding the Discord bot token |
| `ARIEL_DISCORD_GUILD_ID` | unset | The guild `/ariel` is registered in |
| `ARIEL_DISCORD_APPLICATION_ID` | unset | The Discord application answering interactions |
| `ARIEL_GONZALO_TOKEN_FILE` | unset | File holding the gonzalod bearer token |
| `ARIEL_PROSPERO_TOKEN_FILE` | unset | File holding the prosperod API token (`operate` scope) |
| `ARIEL_DASHBOARD_URL` | unset | Linked from every notification |
| `ARIEL_CHANNEL_RELOAD_SECS` | `60` | How often the channel records are re-read |
| `ARIEL_HEALTH_ADDR` | `0.0.0.0:8081` | Where `/healthz` is served |
| `ARIEL_LOG_FORMAT` | `text` | `text` or `json` log lines on stderr |
| `RUST_LOG` | Ariel at `info`, dependencies at `warn` | `tracing` filter directives |

Without `ARIEL_PROSPERO_URL`, `ARIEL_GONZALO_URL` and a complete Discord
configuration, `arield` serves health only and says so. A named credential file
that is missing, unreadable or empty stops it at startup.

## Documentation

- Guide: <https://caliban-ai.github.io/ariel/> (source in [`docs/guide/`](docs/guide/))
- Changelog: [`CHANGELOG.md`](CHANGELOG.md), also
  [in the guide](https://caliban-ai.github.io/ariel/changelog.html).
- API reference: <https://caliban-ai.github.io/ariel/api/>
- Decisions: [`docs/adr/`](docs/adr/README.md), the architecture decision log.
- Discord: [`docs/discord-smoke-test.md`](docs/discord-smoke-test.md).
- Deployment: the `ariel` chart in
  [caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts).
- Tracking: the [caliban-ai board](https://github.com/orgs/caliban-ai/projects/1).

## License

[AGPL-3.0-only](LICENSE), matching the rest of the caliban-ai stack.
