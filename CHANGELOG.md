# Changelog

All notable changes to Ariel are recorded here, in the style of
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Ariel is pre-1.0: a release bumps the **minor** version for new features and the
**patch** version for fixes alone. The crates are not published to crates.io; a
release is the container image `ghcr.io/caliban-ai/ariel`, built for
`linux/amd64` and `linux/arm64` on every `v*` tag.

## [Unreleased]

### Added

- `/ariel channel` shows a channel's configuration, `/ariel configure` changes
  it, and `/ariel invite` mints a one-time link token (#58) — so adding a
  channel or onboarding someone no longer needs access to gonzalod. Both
  changing commands need `admin` and are audited. An invite's reply is always
  private, and on a platform that cannot answer privately it mints nothing;
  nobody can invite above the role they act with. The CLI and the chat command
  now share one parser, so they write the same record.
- `/ariel kill <agent>` stops an agent and `/ariel respawn <agent>` restarts it
  from the prompt it was given, naming the restarted agent's new id (#57). Both
  need an operator in the agent's own workspace: the agent is looked up in the
  fleet first, and authorization is against the workspace it is in. A channel
  with no configuration is refused before the fleet is read, so it cannot be
  used to discover which agents exist.

### Fixed

- `arield` now picks up channel configuration changes while it runs (#56). A
  channel record written after startup was invisible until the process
  restarted — adding a chat channel meant restarting the daemon. It re-reads
  the records every `ARIEL_CHANNEL_RELOAD_SECS` (60 by default), starting,
  stopping and restarting notifiers as records appear, vanish or change. A
  failed read keeps the channels already served.

## [0.2.0] - 2026-09-19

Ariel answers for the fleet, not just about it.

`/ariel status` and `/ariel spawn` are the first commands that act on the fleet,
each authorized on the person's role and the channel's ceiling and audited in
gonzalo. `arield` also has a voice at last: it writes its log to stderr, so a
refused token or a throttled bot is visible instead of silently dropped. The
whole path — link, spawn, notify, status — now runs in CI against prosperod's
and gonzalod's own server code, and was confirmed by hand in a real Discord
guild against the home cluster.

### Added

- `/ariel status` summarizes the workspaces a channel follows, and
  `/ariel spawn <workspace> <prompt>` starts an agent (#20 — [#51]). Both are
  authorized on the person's role and the channel's ceiling; every spawn is
  audited, and prosperod errors reach the person as plain sentences
  ([Chat Commands](docs/guide/src/commands.md)).
- A headless end-to-end smoke test in its own CI job (#21 — [#52]): the bridge
  against
  prosperod's and gonzalod's own server code, with token authentication on and
  prospero's fake caliban for agents, covering linking, a spawn from chat, its
  notification to the finish, and fleet status.

### Fixed

- `arield` now writes its log to stderr, so it shows up in `kubectl logs`. Before
  this, every warning and error was silently dropped, including prosperod
  refusing Ariel's token and the chat platform rate-limiting it (#49 — [#50]).
  `RUST_LOG` sets the level and `ARIEL_LOG_FORMAT=json` switches to JSON lines.
- `arield` checks its prosperod token at startup whenever `ARIEL_PROSPERO_URL`
  is set, even if gonzalod or the chat provider is not configured yet
  (#49 — [#50]).

## [0.1.0] - 2026-09-17

The first release: Ariel notifies chat about the fleet, and knows who is allowed
to ask it for anything.

`arield` watches prosperod's fleet, keeps one live message per agent in every
chat channel configured to follow that agent's workspace, and answers
`/ariel link` so a person's chat account can be tied to their fleet identity. The
`ariel` CLI configures channels and mints link tokens. Everything Ariel
remembers — people, bindings, role grants, channel configuration, link tokens and
the audit trail — lives in gonzalo; Ariel stores nothing of its own.

Not in this release: the fleet commands themselves (`/ariel status`,
`/ariel spawn`), which land next (#20).

### Added

- **The chat provider boundary.** `ChatProvider`, addressing types, a
  provider-neutral `Message`, capabilities with limits and a send budget, and an
  in-memory `ConsoleProvider` for tests, plus a shared contract suite every
  backend must pass (#11, #2 — [#33], [#28]).
- **A Discord backend** on twilight: `/ariel` commands, embeds coloured by
  severity, edits, private replies, and the three-second interaction deadline
  handled by auto-deferring (#14 — [#36]).
- **The prospero client**: fleet polling, per-agent SSE fan-in with gap handling,
  and spawn, kill, respawn and input (#12 — [#27]).
- **Notifications**: one live message per agent, edited in place; a burst of five
  or more agents shares a summary; sends are paced against the provider's own
  budget, and rate limits, lost access and deleted messages are handled (#13,
  #19 — [#34], [#40]).
- **`arield`**, the daemon: reads its configuration from the environment,
  connects to prosperod, gonzalod and Discord, notifies every channel that
  follows an event's workspace, serves `/healthz`, and shuts down cleanly. A
  restart replays nothing (#19, #30 — [#41], [#35]).
- **Access-control records in gonzalo**: typed reads and optimistic writes for
  people, identity bindings, role grants, channel configuration and link tokens,
  with an append-only audit trail (#15 — [#38]).
- **Account linking**: `ariel link new` mints a one-time token and
  `/ariel link <token>` redeems it in chat. Only a hash of the token is stored,
  it works once, and every attempt is audited (#16 — [#45]).
- **Two-key command authorization**: a command runs at the lower of the person's
  role and the channel's ceiling; an unlinked account gets nothing, and every
  mutating command is audited, allowed or denied (#17 — [#44]).
- **Channel configuration from the CLI**: `ariel channel show` and
  `ariel channel set`, audited, with a concurrent edit reported as a conflict
  rather than overwritten (#18 — [#42]).
- **Authentication to prosperod**: a bearer token from
  `ARIEL_PROSPERO_TOKEN_FILE`, sent on every request including the event stream,
  for prosperod v0.8's API authentication (#46 — [#47]).
- **A container image** `ghcr.io/caliban-ai/ariel` for `linux/amd64` and
  `linux/arm64`, a health endpoint, and file-based credentials (#30 — [#35]).
- **The ADR log** (0001–0013) and an mdBook guide published to GitHub Pages
  (#9 — [#24], [#37]).
- **CI**: format, lint, build, test, a build with no chat backend, a guard that
  `ariel-core` pulls in no chat SDK, and an 85% line-coverage floor (#10 —
  [#25]).

[Unreleased]: https://github.com/caliban-ai/ariel/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/caliban-ai/ariel/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/caliban-ai/ariel/releases/tag/v0.1.0
[#24]: https://github.com/caliban-ai/ariel/pull/24
[#25]: https://github.com/caliban-ai/ariel/pull/25
[#27]: https://github.com/caliban-ai/ariel/pull/27
[#28]: https://github.com/caliban-ai/ariel/pull/28
[#33]: https://github.com/caliban-ai/ariel/pull/33
[#34]: https://github.com/caliban-ai/ariel/pull/34
[#35]: https://github.com/caliban-ai/ariel/pull/35
[#36]: https://github.com/caliban-ai/ariel/pull/36
[#37]: https://github.com/caliban-ai/ariel/pull/37
[#38]: https://github.com/caliban-ai/ariel/pull/38
[#40]: https://github.com/caliban-ai/ariel/pull/40
[#41]: https://github.com/caliban-ai/ariel/pull/41
[#42]: https://github.com/caliban-ai/ariel/pull/42
[#44]: https://github.com/caliban-ai/ariel/pull/44
[#45]: https://github.com/caliban-ai/ariel/pull/45
[#47]: https://github.com/caliban-ai/ariel/pull/47
[#50]: https://github.com/caliban-ai/ariel/pull/50
[#51]: https://github.com/caliban-ai/ariel/pull/51
[#52]: https://github.com/caliban-ai/ariel/pull/52
