# Chat Commands

Ariel answers one chat command, `/ariel`, with a subcommand per action. On
Discord each subcommand is a typed slash command with its own options.

| Command | Needs | What it does |
|---|---|---|
| `/ariel link <token>` | nothing | Links your chat account to your fleet identity with a token an administrator gave you ([The `ariel` CLI](./cli.md#link-a-chat-account)). Works in any channel, and the reply is private. |
| `/ariel status` | `viewer`, fleet-wide | Summarizes the workspaces this channel follows: how many agents are active, each workspace's agents by status, and any workspace prosperod cannot reach. Links the dashboard. |
| `/ariel spawn <workspace> <prompt>` | `operator` for that workspace | Starts an agent in `workspace` with `prompt`, and replies with its id. The channel's live notifications take it from there. |

## Who may run what

A command runs with the **lower** of two roles: the one you hold, and the
channel's ceiling ([ADR 0009](./adr/0009-channel-config.md),
[ADR 0012](./adr/0012-command-authorization-and-audit.md)). So an admin in a
channel with a `viewer` ceiling can run `/ariel status` but not `/ariel spawn`.

- **The channel must be configured** with `ariel channel set`. Elsewhere only
  `/ariel link` works.
- **A command acts only on a workspace the channel follows.** A channel following
  `[caliban]` cannot spawn into `prospero`, and its `/ariel status` shows only
  `caliban`.
- **A workspace grant counts only in that workspace.** It lets you spawn there,
  but `/ariel status` covers the fleet the channel follows, so it needs a
  fleet-wide grant.
- **Your chat account must be linked.** An unlinked account is told how to link.

## Replies

A result is posted **publicly**, so the channel sees the fleet and who started
what. A refusal or failure is **private** to the person who asked, and says what
would change the answer: which role is missing, which workspace the channel does
not follow, or that prosperod does not know the workspace.

When prosperod itself refuses Ariel's own token, the person is told an
administrator needs to check it, and `arield` logs an error naming
`ARIEL_PROSPERO_TOKEN_FILE`.

## Audit

Every `/ariel spawn` leaves exactly one audit entry in gonzalo, action
`command.spawn`, target the workspace: `Denied` when refused, and `Succeeded` or
`Failed` according to what prosperod did. A spawn with a missing workspace or
prompt is answered with the usage and not audited, since nothing was attempted.
`/ariel status` is read-only and not audited.
