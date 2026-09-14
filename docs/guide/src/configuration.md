# Configuration

`arield` is configured entirely through environment variables. It has no
configuration file and no command-line options beyond `--help` and `--version`.

## Reference

| Variable | Default | Meaning |
|---|---|---|
| `ARIEL_HEALTH_ADDR` | `0.0.0.0:8081` | Socket address `/healthz` is served on. A value that is not a socket address (for example a bare port) stops `arield` at startup. |
| `ARIEL_DISCORD_TOKEN_FILE` | unset | Path to a file holding the Discord bot token. |
| `ARIEL_GONZALO_TOKEN_FILE` | unset | Path to a file holding `arield`'s bearer token for gonzalod. |

Both credential files are **read and validated at startup but not used yet**:
`arield` does not connect to Discord or gonzalod until the daemon wiring lands
([#19](https://github.com/caliban-ai/ariel/issues/19)). Settings the wiring will
need, such as the prosperod and gonzalod URLs and the Discord guild and
application IDs, do not exist yet.

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
