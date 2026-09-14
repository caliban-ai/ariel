# Discord backend smoke test

The Discord backend's contract tests run against a stub of Discord's REST API,
and never open a real Gateway connection (ADR 0010). This manual test checks
the parts they cannot: a real bot token, slash command registration, posting,
the Gateway connection, and answering an interaction inside Discord's
three-second deadline.

## One-time setup

1. In the [Discord developer portal](https://discord.com/developers/applications),
   create an application and add a bot to it.
2. Copy:
   - the bot token (Bot → Reset Token);
   - the application ID (General Information).
3. Create a test guild, or use one you administer, and enable Developer Mode in
   Discord's settings so you can copy IDs.
4. Invite the bot with the `bot` and `applications.commands` scopes and these
   permissions: View Channels, Send Messages, Embed Links, Create Public Threads,
   Send Messages in Threads. The portal's OAuth2 URL generator builds the link.
5. Copy the guild ID and the ID of a channel the bot can post in.

No privileged Gateway intents are needed: interactions arrive without them.

## Run

```sh
export DISCORD_TOKEN='...'
export DISCORD_APPLICATION_ID='...'
export DISCORD_GUILD_ID='...'
export DISCORD_CHANNEL_ID='...'
cargo run -p ariel-discord --example smoke
```

Keep the token out of shell history and never commit it.

## Expected results

| Step | Pass if |
|---|---|
| Registration | The example prints `registered /ariel status in guild …`, and `/ariel status` appears in the guild's slash command list within a few seconds (guild commands update immediately). |
| Posting | `ariel smoke: connected` appears in the channel as an embed, and the example prints `posted message …`. |
| Gateway | The example prints `listening; …` with no Gateway errors logged. |
| Interaction | Running `/ariel status` shows `ariel smoke: ok` in the channel, and the example prints `received /ariel status …` then `replied: Ok(…)`. Discord never shows "The application did not respond". |

Stop the example with Ctrl-C.

## Recording a run

Note the date, the ariel commit, and any step that failed, in the pull request
or ticket that needed the smoke test.
