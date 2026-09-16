# Status & Roadmap

Ariel is pre-release: workspace version `0.1.0`, no tagged release, and no
published container image yet. This page lists what the code does today, what is
next, and what is blocked. Work is tracked on the
[caliban-ai board](https://github.com/orgs/caliban-ai/projects/1) under the MVP
epic, [caliban-ai/ariel#22](https://github.com/caliban-ai/ariel/issues/22).

## Capability layers

Ariel is designed as one bridge at four depths, each shippable on its own.

| Layer | Direction | What it does | State |
|---|---|---|---|
| Notifications | out | Agent started, changed status, finished | **Building blocks done.** Fleet watcher and renderer exist; the paced notifier and daemon wiring are next (#19). |
| ChatOps | both | Slash commands to list, spawn, kill, and restart agents | **Plumbing done.** Discord registers and receives `/ariel` subcommands, and the prospero client can spawn, kill and respawn. No commands are defined yet (#20). |
| Approvals | both, narrow | Approve or deny a risky action with buttons | **Designed, deferred** (#6). Blocked on upstream caliban and prospero work. |
| Conversational | both, full | A chat thread is an agent session | **Designed, deferred** (#7). |

Commands are to be authorized on two keys: a person's role and a ceiling set on
the channel, the lower of the two winning
([ADR 0009](./adr/0009-channel-config.md)). That authorization is planned (#17).

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

- Reads `ARIEL_DISCORD_TOKEN_FILE`, `ARIEL_GONZALO_TOKEN_FILE` and
  `ARIEL_HEALTH_ADDR`, failing at startup on an unreadable or empty credential
  file ([Configuration](./configuration.md)).
- Prints the chat providers compiled into the build.
- Serves `GET /healthz` and shuts down cleanly on Ctrl-C or SIGTERM.

**`ariel`** (CLI): `--version` and `--help` only.

**Build and release**

- CI: `cargo fmt --check`, clippy with `-D warnings`, build, test, a
  `--no-default-features` build of `ariel-daemon`, a check that `ariel-core` pulls
  in no chat SDK, and an 85% line-coverage floor (`scripts/coverage.sh`).
- `Dockerfile` and a release workflow that builds `ghcr.io/caliban-ai/ariel` for
  `linux/amd64` and `linux/arm64`, validating on pull requests and pushing on `v*`
  tags.

## Next: the MVP walking skeleton

One thin thread through every seam, Discord only.

| Issue | Work | Blocked by |
|---|---|---|
| [#19](https://github.com/caliban-ai/ariel/issues/19) | Wire `arield`: Discord, prosperod and gonzalod connections, fleet notifications to configured channels | — |
| [#16](https://github.com/caliban-ai/ariel/issues/16) | Account linking: `ariel link new` and `/ariel link` | gonzalod auth |
| [#17](https://github.com/caliban-ai/ariel/issues/17) | Two-key command authorization and audit trail | gonzalod auth |
| [#18](https://github.com/caliban-ai/ariel/issues/18) | `ariel` CLI for channel configuration | — |
| [#20](https://github.com/caliban-ai/ariel/issues/20) | `/ariel status` and `/ariel spawn` | #16, #17, #19 |
| [#21](https://github.com/caliban-ai/ariel/issues/21) | Headless end-to-end smoke with real prosperod and gonzalod | #19, #20 |

Not yet decided or built, and not scheduled: the Slack and Teams backends, core
message fallbacks (truncation, dropping actions when a platform has no buttons),
and a published release.

## Blocked upstream

- **A gonzalo release.** The access-control record kinds landed in gonzalo
  ([caliban-ai/gonzalo#296](https://github.com/caliban-ai/gonzalo/pull/296),
  with channel configuration completed in
  [#297](https://github.com/caliban-ai/gonzalo/pull/297)) but
  are not released yet, so Ariel pins gonzalo to that commit from git. Ariel
  moves to the crates.io release once one carries them.
- **gonzalod authentication.** Account linking and authorization wait until
  gonzalod runs with auth on in the deployment
  ([ADR 0008](./adr/0008-secrets-deployment-and-network-boundary.md)).
- **Approvals** need a permission-request event from caliban and prospero.
