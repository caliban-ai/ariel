# Status & Roadmap

Ariel is pre-1.0. The current release is **v0.3.0**, published as
`ghcr.io/caliban-ai/ariel:0.3.0` for `linux/amd64` and `linux/arm64`; see the
[changelog](./changelog.md). This page lists what the code does today, what is
next, and what is deferred. The MVP walking skeleton,
[caliban-ai/ariel#22](https://github.com/caliban-ai/ariel/issues/22), is
complete; remaining work is tracked on the
[caliban-ai board](https://github.com/orgs/caliban-ai/projects/1).

## Capability layers

Ariel is designed as one bridge at four depths, each shippable on its own.

| Layer | Direction | What it does | State |
|---|---|---|---|
| Notifications | out | Agent started, changed status, finished | **Built.** `arield` watches the fleet and posts a live message per agent to every channel configured to follow its workspace, naming the person who asked for it. |
| ChatOps | both | Slash commands to list, spawn, kill, and restart agents | **Built.** Eight commands: `link`, `status`, `spawn`, `kill`, `respawn`, `channel`, `configure`, `invite` ([Chat Commands](./commands.md)). |
| Approvals | both, narrow | Approve or deny a risky action with buttons | **Designed, deferred** ([#6](https://github.com/caliban-ai/ariel/issues/6)). Waiting on a permission-request event from caliban and prospero. |
| Conversational | both, full | A chat thread is an agent session | **Designed, deferred** ([#7](https://github.com/caliban-ai/ariel/issues/7)). |

Commands are authorized on two keys: a person's role and a ceiling set on the
channel, the lower of the two winning ([ADR 0009](./adr/0009-channel-config.md)).
The authorization and its audit trail are built
([ADR 0012](./adr/0012-command-authorization-and-audit.md)), and every command
but `/ariel link` goes through them.

## Built

**`ariel-core`**

- `ChatProvider` trait, addressing types (`ChannelRef`, `UserRef`, `ThreadRef`,
  `MessageRef`), the provider-neutral `Message`, `CommandSpec` and `Role`,
  `Capabilities` with `Limits` and `SendBudget`, and `ProviderError`
  ([ADR 0006](./adr/0006-chat-provider-trait.md)).
- `ConsoleProvider`: an in-memory provider that records every outbound call and
  accepts injected commands, with configurable capabilities.
- The shared provider contract suite, behind the `contract-tests` feature, run
  against both `ConsoleProvider` and the Discord backend.
- `ProsperoClient` for prosperod's fleet, spawn, kill, respawn, input, end-input
  and stream routes; an incremental SSE decoder with gap handling; and
  `FleetWatcher`, the fleet-wide event feed.
- Mirrored prospero wire types pinned by golden fixtures from prospero v0.8.1,
  plus v0.9's `on_behalf_of` on the event envelope, pinned by its own wire tests
  ([ADR 0005](./adr/0005-mirror-prospero-wire-types.md)). Bearer-token
  authentication to prosperod, sent on every request including the event stream
  ([ADR 0013](./adr/0013-ariel-authenticates-to-prosperod.md)). Both `http` and
  `https` work, TLS through rustls against the platform's trust store.
- `ProsperoClient::on_behalf_of`, which names the person a request acts for in
  prospero v0.9's `X-Prospero-On-Behalf-Of` header. A value prosperod would
  answer `400` to — blank, over 128 characters, or carrying control characters
  — is dropped rather than failing the request it rode on.
- The renderer: `AgentView`, `render_agent`, `render_summary`. An agent's
  message carries a `started by` field when the spawning client named a person
  ([Who started an agent](./commands.md#who-started-an-agent)); one started
  outside Ariel renders exactly as it did before.
- The notifier: one live message per agent, edited in place; burst summaries;
  routing by what a channel follows and its notify preset; and sends paced
  against the provider's own budget, with rate limits, lost access and deleted
  messages handled ([ADR 0007](./adr/0007-notifications-live-messages-and-pacing.md),
  [ADR 0009](./adr/0009-channel-config.md)). It also resolves the person id an
  event carries to that person's display name, reading gonzalo once per person
  and not caching a miss, so someone created later still resolves.
- `Records`, the gonzalo client for access-control records: typed create, read,
  update, delete and list for people, identity bindings, role grants, channel
  configuration and link tokens, and append-only audit entries, over gonzalod
  (`ServerStore`) or a local `FsStore`. A lost write race is an ordinary
  `Write::Conflict` carrying the winning record
  ([gonzalo compatibility](./configuration.md#gonzalo-compatibility)).

**`ariel-discord`** ([ADR 0010](./adr/0010-discord-library-twilight.md))

- Registers one guild `/ariel` command with a subcommand per `CommandSpec`
  (string options).
- Posts messages as embeds colored by severity; edits them; opens direct
  messages; starts threads from a message.
- Reads the Gateway with no privileged intents and turns `/ariel` interactions
  into `Inbound::Command`s.
- Answers interactions, auto-deferring after 2 seconds to meet Discord's
  3-second deadline, and supports ephemeral (private) replies.
- Advertises every capability except buttons and reading thread replies.
- Verified by the contract suite against a stub of Discord's REST API, plus a
  manual [smoke test](./discord.md) against a real guild.

**`arield`**

- Reads its configuration from the environment, failing at startup on an
  unreadable or empty credential file, a bad address, a non-URL dashboard or a
  non-numeric Discord ID ([Configuration](./configuration.md)).
- Connects to prosperod, gonzalod and Discord, reads the channel configuration
  records belonging to the running provider, and notifies each channel that
  follows an event's workspace. Without those settings it serves health only.
- Re-reads the channel records while running
  (`ARIEL_CHANNEL_RELOAD_SECS`, 60s by default), so a channel added, retired or
  re-scoped takes effect without a restart. A failed read keeps the channels
  already served rather than dropping them.
- Registers every `/ariel` command: `link`, `status`, `spawn`, `kill`,
  `respawn`, `channel`, `configure` and `invite`. Link answers privately,
  linking the account and granting the token's role; the rest are authorized on
  two keys, and every command that changes something is audited
  ([Chat Commands](./commands.md)). `spawn`, `kill` and `respawn` tell
  prosperod which person they act for, so the events they cause name that
  person and not only Ariel's token.
- Does not replay what it missed across a restart
  ([ADR 0011](./adr/0011-no-replay-after-a-restart.md)).
- Writes its log to **stderr**, so `kubectl logs` shows a refused token or a
  throttled bot: `RUST_LOG` sets the level and `ARIEL_LOG_FORMAT=json` switches
  to one JSON object per line. Logging is installed before anything else can
  fail, and the startup line names the version and the chat providers compiled
  into the build.
- Checks its prosperod token as soon as `ARIEL_PROSPERO_URL` is set, in the
  background so a slow prosperod never holds up the health endpoint.
- Serves `GET /healthz` and shuts down cleanly on Ctrl-C or SIGTERM.

**`ariel`** (CLI)

- `ariel channel show` and `ariel channel set` manage a channel's configuration
  record: what it follows, how much it hears and its command ceiling
  ([The `ariel` CLI](./cli.md)). Changes are audited, and a concurrent edit is
  reported as a conflict instead of overwriting what is stored.
- `ariel link new` mints a one-time link token that grants a role when redeemed
  in chat with `/ariel link`; only its hash is stored, and it works once
  ([The `ariel` CLI](./cli.md#link-a-chat-account)).
- Acts on gonzalod, or on a local gonzalo store with `--store`.

**Build and release**

- CI: `cargo fmt --check`, clippy with `-D warnings`, build, test, a
  `--no-default-features` build of `ariel-daemon`, a check that `ariel-core` pulls
  in no chat SDK, and an 85% line-coverage floor (`scripts/coverage.sh`). A
  docs-only change skips the Rust jobs.
- A headless end-to-end smoke test (`crates/e2e`), run in its own CI job: the
  bridge against prosperod's (v0.9.0) and gonzalod's own server code, both with
  token authentication on, and prospero's fake caliban standing in for agents. A
  person links, spawns an agent from chat, sees its notification through to
  the finish — including the `started by` field resolved to the display name
  linking gave their account — and asks for the fleet status; the audit trail is
  checked in gonzalo. No network beyond loopback and no model API keys.
- `Dockerfile` and a release workflow that builds `ghcr.io/caliban-ai/ariel` for
  `linux/amd64` and `linux/arm64` on **native runners** (no QEMU), validating on
  pull requests and pushing a multi-arch manifest on `v*` tags, tagged by
  version, by `sha-<commit>`, and `latest`.
- Deployment: the `ariel` chart in
  [caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts), tracking
  the published image.

## Next

The MVP walking skeleton ([#22](https://github.com/caliban-ai/ariel/issues/22))
is complete: the path from link through spawn, notification and status runs in CI
against prosperod's and gonzalod's own server code, and was confirmed by hand in
a real Discord guild against the home cluster at v0.2.0. What is open is
refinement rather than foundation:

- **One fleet-wide event stream** instead of polling plus one SSE stream per
  agent ([#55](https://github.com/caliban-ai/ariel/issues/55)). `FleetWatcher`
  polls `GET /api/fleet` and opens one SSE connection per agent, which costs a
  connection each and can miss an agent that starts and finishes between two
  polls. prospero **v0.9.0** ships `GET /api/fleet/stream`, whose SSE `id:` is a
  fleet-wide cursor, so the shim can go — the blocker was a prospero release and
  that release is out. Not started.
- **Require the end-to-end smoke job as a status check**
  ([#61](https://github.com/caliban-ai/ariel/issues/61)).
- Housekeeping: pin the Rust toolchain
  ([#68](https://github.com/caliban-ai/ariel/issues/68)) and remove the tracked
  `.profraw` files ([#69](https://github.com/caliban-ai/ariel/issues/69)).

Not yet decided or built, and not scheduled: the **Slack and Teams backends**,
and core message fallbacks (truncation, dropping actions when a platform has no
buttons).

## Deferred by design

- **Approvals** ([#6](https://github.com/caliban-ai/ariel/issues/6)): designed,
  and waiting on a permission-request event from caliban and prospero. Until one
  exists there is nothing for Ariel to ask about. Discord's buttons are the one
  provider capability `ariel-discord` does not yet advertise.
- **Conversational threads** ([#7](https://github.com/caliban-ai/ariel/issues/7)):
  designed — a chat thread as an agent session — and deliberately after the
  first three layers. Reading thread replies is the other capability the Discord
  backend does not advertise yet.

## Operating requirements

Not blockers in Ariel, but things the deployment has to supply:

- **gonzalod 0.7.0 or newer, with authentication on.** The access-control record
  kinds ship from 0.7.0 (gonzalo ADR 0022 and 0023); 0.6.0 and earlier cannot
  decode them. Linking writes identity records, so a gonzalod with auth off
  would let any pod grant itself a role
  ([ADR 0008](./adr/0008-secrets-deployment-and-network-boundary.md),
  [gonzalo compatibility](./configuration.md#gonzalo-compatibility)).
- **A prosperod API token** once prospero v0.8 or newer runs with API
  authentication on, with the `operate` scope for the commands that act
  ([ADR 0013](./adr/0013-ariel-authenticates-to-prosperod.md)).
- **prosperod v0.9 or newer to see who started an agent.** The attribution
  rides on a header v0.9 introduced; an older daemon ignores it and the
  `started by` field simply does not appear. Nothing else degrades
  ([Who started an agent](./commands.md#who-started-an-agent)).

The gonzalo work Ariel's identity layer waited on —
[gonzalo#277](https://github.com/caliban-ai/gonzalo/issues/277) and
[#278](https://github.com/caliban-ai/gonzalo/issues/278) — landed in gonzalo
v0.7.0, and Ariel depends on the published crate.
