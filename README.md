# Ariel

[![license](https://img.shields.io/badge/license-AGPL--3.0--only-blue)](LICENSE)

Ariel is the **chat bridge** for the [Caliban](https://github.com/caliban-ai/caliban)
agent fleet. It brings fleet notifications, commands, approvals, and conversation
into the chat platforms where teams already work: Discord first, then Slack and
Microsoft Teams.

In *The Tempest*, Ariel is the spirit Prospero sends to carry messages. Here it
carries them between people in chat and the fleet that
[Prospero](https://github.com/caliban-ai/prospero) runs.

> **Status: design.** Nothing is implemented yet. The design is open, and the
> identity layer is blocked on upstream work in gonzalo
> ([#277](https://github.com/caliban-ai/gonzalo/issues/277),
> [#278](https://github.com/caliban-ai/gonzalo/issues/278)).

## How it is meant to work

Ariel is a standalone service, a sibling to prospero and gonzalo, so that chat
platform SDKs, OAuth, signature verification, and rate limiting stay behind one
boundary.

```
Discord / Slack / Teams
        │  gateway socket, interactions
        ▼
┌───────────────────────────────────────────────────────┐
│ arield                                                 │
│  ChatProvider trait · router · renderer · auth ·       │
│  session bridge                                        │
└──────────┬──────────────────────────────┬──────────────┘
           │ HTTP + SSE                   │ records
           ▼                              ▼
       prosperod                       gonzalod
  fleet events, spawn,          people, roles, channel
  kill, input                   config, audit
```

- **Through prospero.** All fleet control and events go over prospero's public
  HTTP API. Ariel does not depend on prospero crates.
- **With gonzalo.** Identity, role grants, channel configuration, and the audit
  trail are gonzalo records. Ariel stores nothing of its own.
- **To caliban** only indirectly, through prospero.

Chat platforms sit behind one `ChatProvider` trait, and each backend is a
feature-gated crate. A build without the `slack` feature cannot load Slack.

## Capabilities

One bridge at four depths, each shippable on its own:

| Layer | Direction | What it does |
|---|---|---|
| Notifications | out | Agent started, changed status, finished |
| Approvals | both, narrow | Approve or deny a risky action with buttons. Blocked on upstream caliban and prospero work |
| ChatOps | both | Slash commands to list, spawn, kill, and restart agents |
| Conversational | both, full | A chat thread is an agent session |

Commands are authorized on two keys: a person's role and a ceiling set on the
channel. The lower of the two wins.

## Design

- Design spec: `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md`
  in the caliban-ai umbrella workspace.
- Decisions: [`docs/adr/`](docs/adr/README.md), the architecture decision log.
- Tracking: [caliban-ai/prospero#67](https://github.com/caliban-ai/prospero/issues/67).

## License

[AGPL-3.0-only](LICENSE), matching the rest of the caliban-ai stack.
