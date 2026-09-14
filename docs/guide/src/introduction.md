# ariel

Ariel is the **chat bridge** for the [Caliban](https://github.com/caliban-ai/caliban)
agent fleet. It brings fleet notifications, commands, approvals, and conversation
into the chat platforms where teams already work: Discord first, then Slack and
Microsoft Teams.

In *The Tempest*, Ariel is the spirit Prospero sends to carry messages. Here it
carries them between people in chat and the fleet that
[Prospero](https://github.com/caliban-ai/prospero) runs.

## Where it stands

Ariel is **pre-release** (workspace version `0.1.0`, no tagged release yet). The
foundation is built and tested; the pieces are not yet wired into a running
bridge. Today the repository has:

- the provider-neutral `ChatProvider` trait, an in-memory `ConsoleProvider`, and a
  shared contract suite every backend runs;
- a typed client for prosperod's HTTP and SSE API, and a `FleetWatcher` that turns
  prospero's per-agent streams into one fleet-wide event feed;
- a renderer that folds an agent's events into its current state and renders it as
  a chat message;
- a Discord backend on twilight that registers `/ariel` slash commands, posts and
  edits embeds, and receives commands over the Gateway;
- an `arield` daemon that reads its credentials from files and serves `/healthz`,
  and a container image build.

`arield` does not yet connect to Discord, prosperod, or gonzalod, and no chat
commands exist. See [Status & Roadmap](./status.md) for exactly what is built,
what is next, and what is blocked.

## Reading this guide

- [Architecture & Couplings](./architecture.md): how Ariel sits beside prospero
  and gonzalo, and how the crates divide the work.
- [Getting Started](./getting-started.md): build, test, and run `arield` locally
  or in a container.
- [Configuration](./configuration.md): every setting `arield` reads today.
- [Discord Setup](./discord.md): create a bot and run the manual smoke test.
- [Architecture Decisions](./adr/index.md): the reasoning behind the design.
