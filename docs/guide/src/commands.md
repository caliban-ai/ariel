# Chat Commands

Ariel answers one chat command, `/ariel`, with a subcommand per action. On
Discord each subcommand is a typed slash command with its own options.

| Command | Needs | What it does |
|---|---|---|
| `/ariel link <token>` | nothing | Links your chat account to your fleet identity with a token an administrator gave you ([The `ariel` CLI](./cli.md#link-a-chat-account)). Works in any channel, and the reply is private. |
| `/ariel status` | `viewer`, fleet-wide | Summarizes the workspaces this channel follows: how many agents are active, each workspace's agents by status, and any workspace prosperod cannot reach. Links the dashboard. |
| `/ariel spawn <workspace> <prompt>` | `operator` for that workspace | Starts an agent in `workspace` with `prompt`, and replies with its id. The channel's live notifications take it from there. |
| `/ariel kill <agent>` | `operator` for the agent's workspace | Stops an agent. |
| `/ariel respawn <agent>` | `operator` for the agent's workspace | Restarts an agent from the prompt it was given. The restarted agent has a **new id**, which the reply names. |
| `/ariel channel` | `viewer` | Shows what this channel follows, hears and allows. |
| `/ariel configure [follows] [notify] [ceiling]` | `admin` | Changes this channel's configuration, the same fields as `ariel channel set`. Audited, and a concurrent edit is reported rather than overwritten. |
| `/ariel invite <role> [workspace] [hours]` | `admin` | Mints a one-time link token to hand to someone, who redeems it with `/ariel link`. |

`kill` and `respawn` name an agent rather than a workspace, so Ariel looks the
agent up in the fleet first and authorizes against the workspace it is in. An
agent the fleet does not have is refused without asking prosperod to do
anything, and nothing is audited, because nothing was attempted.

A channel with no configuration is refused before the fleet is read at all, so
it cannot be used to find out which agents exist: the refusal reads the same
whether or not the agent is real.

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

## Administering a channel from chat

`/ariel configure` and `/ariel invite` do from chat what `ariel channel set` and
`ariel link new` do from a terminal, so adding a channel or onboarding someone
no longer needs access to gonzalod ([The `ariel` CLI](./cli.md)). Both need
`admin`, so a channel whose ceiling is `operator` cannot reconfigure itself —
which is the point of the ceiling.

Two things worth knowing:

- **Bootstrapping is still a CLI job.** Commands only work in a configured
  channel, so the first channel has to be created with `ariel channel set`.
- **An invite's token is private or it does not exist.** The reply carries a
  live secret, so it is always private. On a chat platform that can neither
  reply ephemerally nor send a direct message, `/ariel invite` refuses and mints
  nothing, rather than minting a token Ariel cannot hand over safely. Nobody can
  invite at a role above the one they act with in that channel.

## Replies

A result is posted **publicly**, so the channel sees the fleet and who started
what. A refusal or failure is **private** to the person who asked, and says what
would change the answer: which role is missing, which workspace the channel does
not follow, or that prosperod does not know the workspace.

When prosperod itself refuses Ariel's own token, the person is told an
administrator needs to check it, and `arield` logs an error naming
`ARIEL_PROSPERO_TOKEN_FILE`.

## Audit

Every `/ariel spawn`, `/ariel kill` and `/ariel respawn` leaves exactly one
audit entry in gonzalo — action `command.spawn`, `command.kill` or
`command.respawn`, target the workspace: `Denied` when refused, and `Succeeded` or
`Failed` according to what prosperod did. A command with a missing argument, or
one naming an agent the fleet does not have, is answered with the usage or a
refusal and not audited, since nothing was attempted.
`/ariel status` is read-only and not audited.
