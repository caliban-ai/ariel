# ariel

Ariel is the **chat bridge** for the [Caliban](https://github.com/caliban-ai/caliban)
agent fleet. It brings fleet notifications, commands, approvals, and conversation
into the chat platforms where teams already work: Discord first, then Slack and
Microsoft Teams.

In *The Tempest*, Ariel is the spirit Prospero sends to carry messages. Here it
carries them between people in chat and the fleet that
[Prospero](https://github.com/caliban-ai/prospero) runs.

## Where it stands

Ariel is pre-1.0. The current release is **v0.3.0**, published as the container
image `ghcr.io/caliban-ai/ariel:0.3.0` for `linux/amd64` and `linux/arm64`. It is
a running bridge on Discord:

- `arield` watches prosperod's fleet and keeps **one live message per agent** in
  every chat channel configured to follow that agent's workspace, editing it in
  place as the agent changes, collapsing bursts into a summary and pacing sends
  against the platform's own budget — and, for an agent started from chat,
  naming the person who asked for it
  ([Who started an agent](./commands.md#who-started-an-agent));
- it answers **eight `/ariel` commands** — `link`, `status`, `spawn`, `kill`,
  `respawn`, `channel`, `configure` and `invite` — each authorized on the lower
  of the person's role and the channel's ceiling, each mutating one audited in
  gonzalo ([Chat Commands](./commands.md));
- **identity, role grants, channel configuration, link tokens and the audit
  trail are gonzalo records**; Ariel stores nothing of its own;
- channel configuration is re-read **while the daemon runs**, so adding or
  retiring a channel needs no restart;
- the `ariel` CLI configures channels and mints link tokens
  ([The `ariel` CLI](./cli.md)).

Not built: the **approvals** layer and **conversational threads** are designed
and deferred, and the **Slack and Teams** backends are not written. See
[Status & Roadmap](./status.md) for exactly what is built, what is next, and what
is blocked.

## Reading this guide

- [Architecture & Couplings](./architecture.md): how Ariel sits beside prospero
  and gonzalo, and how the crates divide the work.
- [Status & Roadmap](./status.md): what is built, next, deferred and blocked.
- [Getting Started](./getting-started.md): build, test, and run `arield` from
  source or from the published image.
- [Configuration](./configuration.md): every setting `arield` reads.
- [The `ariel` CLI](./cli.md): channel configuration and link tokens.
- [Chat Commands](./commands.md): the `/ariel` commands and who may run them.
- [Discord Setup](./discord.md): create a bot, register the commands, and run
  the manual smoke test.
- [Changelog](./changelog.md): what changed in each release.
- [Architecture Decisions](./adr/index.md): the reasoning behind the design.
