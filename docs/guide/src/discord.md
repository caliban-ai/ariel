# Discord Setup

Discord is Ariel's first chat platform. The backend, `ariel-discord`, is built on
twilight ([ADR 0010](./adr/0010-discord-library-twilight.md)) and compiled into
`arield` by the default `discord` feature.

`arield` starts the backend as soon as `ARIEL_DISCORD_TOKEN_FILE`,
`ARIEL_DISCORD_GUILD_ID` and `ARIEL_DISCORD_APPLICATION_ID` are all set
([Configuration](./configuration.md)): it registers the `/ariel` commands,
reads interactions over the Gateway, and posts notifications. The smoke test
below exercises the backend on its own against a real guild, without a
prosperod or gonzalod.

## What the backend does

- **Commands.** Registers one guild slash command, `/ariel`, with a subcommand
  for each command the core declares — `link`, `status`, `spawn`, `kill`,
  `respawn`, `channel`, `configure` and `invite` ([Chat Commands](./commands.md)).
  Guild commands update immediately.
- **Receiving.** Connects to the Gateway with no privileged intents, since
  interactions arrive without them, and turns `/ariel` interactions into commands
  for the core. Other interactions are ignored.
- **Replying.** Answers within Discord's 3-second interaction deadline. If the
  core has not replied after 2 seconds, the backend defers on its behalf, then
  edits the deferred response or sends a follow-up. Private replies are
  ephemeral.
- **Posting.** Sends messages as embeds (title, description, inline fields, link)
  colored by severity, edits them in place, opens direct messages, and starts
  threads from a message.
- **Limits.** Advertises Discord's embed limits (256-character title,
  4096-character description, 25 fields of 1024 characters, 6000 characters in
  total) and a send budget of a burst of 5 and 3600 per hour. twilight's REST
  client handles Discord's own rate-limit buckets and retries.
- **Not supported yet:** buttons and reading thread replies.

## Smoke test

The smoke test is kept in the repository at
[`docs/discord-smoke-test.md`](https://github.com/caliban-ai/ariel/blob/main/docs/discord-smoke-test.md)
and included here.

{{#include ../../discord-smoke-test.md:3:}}
