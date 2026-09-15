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

## gonzalo compatibility

Ariel keeps its people, bindings, role grants, channel configuration, link tokens
and audit trail as gonzalo records
([ADR 0003](./adr/0003-no-state-of-its-own.md)). Those record kinds are defined by
gonzalo ADR 0022.

- **Minimum gonzalo:** commit `f537da7`
  ([caliban-ai/gonzalo#296](https://github.com/caliban-ai/gonzalo/pull/296)), the
  first to carry the access-control kinds. No release includes it yet: gonzalo
  0.6.0 and earlier do not, and Ariel depends on gonzalo from git at that commit
  until one does.
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
