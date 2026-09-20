# Status & Roadmap

Ariel is pre-1.0. The current release is **v0.2.0**, published as
`ghcr.io/caliban-ai/ariel:0.2.0` for `linux/amd64` and `linux/arm64`; see the
[changelog](https://github.com/caliban-ai/ariel/blob/main/CHANGELOG.md). This page
lists what the code does today, what is next, and what is blocked. Work is tracked
on the
[caliban-ai board](https://github.com/orgs/caliban-ai/projects/1) under the MVP
epic, [caliban-ai/ariel#22](https://github.com/caliban-ai/ariel/issues/22).

## Capability layers

Ariel is designed as one bridge at four depths, each shippable on its own.

| Layer | Direction | What it does | State |
|---|---|---|---|
| Notifications | out | Agent started, changed status, finished | **Built.** `arield` watches the fleet and posts a live message per agent to every channel configured to follow its workspace. |
| ChatOps | both | Slash commands to list, spawn, kill, and restart agents | **Started.** `/ariel link`, `/ariel status` and `/ariel spawn` work ([Chat Commands](./commands.md)); kill and restart are not built yet. |
| Approvals | both, narrow | Approve or deny a risky action with buttons | **Designed, deferred** (#6). Blocked on upstream caliban and prospero work. |
| Conversational | both, full | A chat thread is an agent session | **Designed, deferred** (#7). |

Commands are authorized on two keys: a person's role and a ceiling set on the
channel, the lower of the two winning ([ADR 0009](./adr/0009-channel-config.md)).
The authorization and its audit trail are built
([ADR 0012](./adr/0012-command-authorization-and-audit.md)), and `/ariel status`
and `/ariel spawn` go through them.

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
- Mirrored prospero wire types pinned by golden fixtures from prospero v0.7.0
  ([ADR 0005](./adr/0005-mirror-prospero-wire-types.md)).
- The renderer: `AgentView`, `render_agent`, `render_summary`.
- The notifier: one live message per agent, edited in place; burst summaries;
  routing by what a channel follows and its notify preset; and sends paced
  against the provider's own budget, with rate limits, lost access and deleted
  messages handled ([ADR 0007](./adr/0007-notifications-live-messages-and-pacing.md),
  [ADR 0009](./adr/0009-channel-config.md)).
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
- Registers `/ariel link`, `/ariel status` and `/ariel spawn`. Link answers
  privately, linking the account and granting the token's role; status and spawn
  are authorized on two keys, and every spawn is audited
  ([Chat Commands](./commands.md)).
- Does not replay what it missed across a restart
  ([ADR 0011](./adr/0011-no-replay-after-a-restart.md)).
- Prints the chat providers compiled into the build.
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
  in no chat SDK, and an 85% line-coverage floor (`scripts/coverage.sh`).
- A headless end-to-end smoke test (`crates/e2e`), run in its own CI job: the
  bridge against prosperod's and gonzalod's own server code, both with token
  authentication on, and prospero's fake caliban standing in for agents. A
  person links, spawns an agent from chat, sees its notification through to
  the finish, and asks for the fleet status; the audit trail is checked in
  gonzalo. No network beyond loopback and no model API keys.
- `Dockerfile` and a release workflow that builds `ghcr.io/caliban-ai/ariel` for
  `linux/amd64` and `linux/arm64`, validating on pull requests and pushing on `v*`
  tags.

## Next

The MVP walking skeleton
([#22](https://github.com/caliban-ai/ariel/issues/22)) is built; what remains
is confirming `/ariel status` and `/ariel spawn` in a real guild.

Not yet decided or built, and not scheduled: the Slack and Teams backends, and
core message fallbacks (truncation, dropping actions when a platform has no
buttons).

## Blocked upstream

- **gonzalod authentication.** Account linking and authorization wait until
  gonzalod runs with auth on in the deployment
  ([ADR 0008](./adr/0008-secrets-deployment-and-network-boundary.md)).
- **Approvals** need a permission-request event from caliban and prospero.
