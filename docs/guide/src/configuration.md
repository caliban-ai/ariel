# Configuration

`arield` is configured entirely through environment variables. It has no
configuration file and no command-line options beyond `--help` and `--version`.

## Reference

| Variable | Default | Meaning |
|---|---|---|
| `ARIEL_HEALTH_ADDR` | `0.0.0.0:8081` | Socket address `/healthz` is served on. A value that is not a socket address (for example a bare port) stops `arield` at startup. |
| `ARIEL_DISCORD_TOKEN_FILE` | unset | Path to a file holding the Discord bot token. |
| `ARIEL_GONZALO_TOKEN_FILE` | unset | Path to a file holding `arield`'s bearer token for gonzalod. |
| `ARIEL_PROSPERO_TOKEN_FILE` | unset | Path to a file holding `arield`'s API token for prosperod. Required once prosperod runs with API authentication on (prospero v0.8+); use a token with `operate` scope ([ADR 0013](./adr/0013-ariel-authenticates-to-prosperod.md)). Without it, requests carry no token. |
| `ARIEL_PROSPERO_URL` | unset | prosperod's base URL, for example `http://prosperod:8080`. |
| `ARIEL_GONZALO_URL` | unset | gonzalod's base URL, where the channel and access-control records live. |
| `ARIEL_DASHBOARD_URL` | unset | Linked from every notification. A value that is not a URL stops `arield` at startup. |
| `ARIEL_CHANNEL_RELOAD_SECS` | `60` | How often `arield` re-reads the channel configuration records, so a channel added, retired or re-scoped takes effect without a restart. A value that is not a whole number of seconds above zero stops `arield` at startup. |
| `RUST_LOG` | `warn,arield=info,ariel_daemon=info,ariel_core=info,ariel_discord=info` | Which log lines `arield` writes to stderr, as [`tracing` filter directives](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html). The default shows Ariel's own `info` lines and only warnings from its dependencies. A filter that does not parse stops `arield` at startup. |
| `ARIEL_LOG_FORMAT` | `text` | `text` for one human-readable line per event, or `json` for one JSON object per line, for log shipping. Any other value stops `arield` at startup. |
| `ARIEL_DISCORD_GUILD_ID` | unset | The guild `/ariel` is registered in. Numeric; anything else stops `arield` at startup. |
| `ARIEL_DISCORD_APPLICATION_ID` | unset | The Discord application answering interactions. Numeric. |

### What `arield` does with less than all of it

`arield` always serves `/healthz`. The bridge itself needs prosperod, gonzalod
and a chat provider:

- **Without `ARIEL_PROSPERO_URL` or `ARIEL_GONZALO_URL`** it serves health only,
  and says so on startup.
- **Without a complete Discord configuration** (token file, guild ID and
  application ID) there is no chat provider, so it serves health only.
- **With all of it**, it reads every channel configuration record belonging to
  the running provider, watches the fleet, and notifies each channel that follows
  an event's workspace. A channel record naming another provider is skipped: it
  belongs to a different backend in the same fleet.
- **With no channel records at all** it runs and notifies nothing, logging a
  warning.

After a restart it does not replay what it missed
([ADR 0011](./adr/0011-no-replay-after-a-restart.md)): agents already finished
are not announced, and each running agent gets a fresh live message.

## Credentials are files

Per [ADR 0008](./adr/0008-secrets-deployment-and-network-boundary.md), a
credential never arrives as a plain environment variable. Each is a file, normally
a mounted Kubernetes Secret, named by an `ARIEL_*_TOKEN_FILE` variable.

- An unset variable means that credential is absent.
- A set variable whose file is missing, unreadable, or empty stops `arield` with a
  non-zero exit. The error names the variable and path, never the contents:

  ```text
  arield: ARIEL_DISCORD_TOKEN_FILE: cannot read /run/secrets/ariel/discord-token: No such file or directory (os error 2)
  ```

- One trailing newline (`\n` or `\r\n`) is removed, so a file written with `echo`
  works.
- Tokens are held in a type that formats as `[redacted]`, so they cannot reach
  logs.

Rotating a token means replacing the file and restarting `arield`.

## gonzalo compatibility

Ariel keeps its people, bindings, role grants, channel configuration, link tokens
and audit trail as gonzalo records
([ADR 0003](./adr/0003-no-state-of-its-own.md)). Those record kinds are defined by
gonzalo ADR 0022, and the channel configuration record by gonzalo ADR 0023.

- **Minimum gonzalo: 0.7.0**, the first release carrying the access-control
  record kinds ([gonzalo v0.7.0](https://github.com/caliban-ai/gonzalo/releases/tag/v0.7.0)).
  0.6.0 and earlier cannot decode them.
- **Upgrade gonzalo first.** A gonzalod, or any gonzalo peer that syncs with it,
  older than that cannot decode these kinds. Upgrade every gonzalo binary that
  will hold or sync Ariel's records before `arield` writes one.

## Health endpoint

`GET /healthz` answers `200` with body `ok`; every other path is `404`. It
reports only that the process is up and serving, and is suitable for liveness and
readiness probes. The container image exposes port 8081.

## Build-time selection

Which chat providers exist in a binary is decided by Cargo features, not
configuration: `ariel-daemon`'s `discord` feature is on by default. `arield`
prints the list at startup, for example `compiled providers: [discord]`, or
`compiled providers: []` for a `--no-default-features` build. Choosing which
compiled provider runs is to be configuration once the daemon is wired.
