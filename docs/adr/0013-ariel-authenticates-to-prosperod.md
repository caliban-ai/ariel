# ADR 0013 · Ariel authenticates to prosperod with a scoped API token

- **Status:** accepted
- **Date:** 2026-09-17
- **Source:** issue #46; revisits [ADR 0008](0008-secrets-deployment-and-network-boundary.md); prospero ADR 0010 (inbound API authentication, shipped in prospero v0.8.0)

## Context

[ADR 0008](0008-secrets-deployment-and-network-boundary.md) was written while
prosperod's API had no authentication. It planned no prospero credential for
Ariel, contained the gap with a NetworkPolicy allow-list, and named prospero adding
API authentication as a reason to revisit it.

That has happened. prospero v0.8.0 added scoped, declarative API tokens (prospero
ADR 0010), and the home cluster's prosperod now runs with them on. Once a tokens
file is configured, **every request needs a token, loopback included**, so an
`arield` that sends none sees `401` on every fleet poll and stream and notifies
nothing.

prosperod's scopes are ordered `read < operate < admin`:

| Scope | Allows |
|---|---|
| `read` | Every GET, including the agent event stream |
| `operate` | `read`, plus spawn, kill, respawn, input, end-input and removing an agent |
| `admin` | `operate`, plus adding, removing and configuring workspaces |

A credential is sent as `Authorization: Bearer <token>`, including on the event
stream. There is no query-parameter form.

## Decision

- **`arield` authenticates to prosperod with its own API token**, named `ariel`, sent
  as a bearer header on every request. It arrives as a file named by
  `ARIEL_PROSPERO_TOKEN_FILE`, like every other credential (ADR 0008): never a plain
  environment variable, never in chart values, and redacted when formatted.
- **The token has `operate` scope**, the least that covers Ariel's commands: spawn
  and kill need it (#20), and notifications need only `read`. Ariel never needs
  `admin`, since it does not change workspaces.
- **Without a token, requests carry none**, so `arield` still works against a
  prosperod with authentication off or older than v0.8.
- **A refused token is a distinct error.** A `401` or `403` becomes
  `ClientError::Auth`, logged as an error naming `ARIEL_PROSPERO_TOKEN_FILE`, because
  retrying cannot fix it. At startup `arield` calls `GET /api/session` and logs the
  token name and scope prosperod sees.
- **prospero's attribution is mirrored.** Events now carry an optional `actor`, the
  token that caused them (ADR 0005). Ariel's spawns will appear as `ariel` in
  prosperod's audit log and events.

## Consequences

- **Positive:** Ariel reaches prosperod as a named, least-privilege principal
  rather than as anonymous network access, and prosperod records which of its
  mutations were Ariel's. A missing or under-scoped token is visible at startup
  instead of as silent missing notifications.
- **Negative:** one more credential to issue, seal and rotate. prosperod checks the
  token's scope, not the person behind a chat command; *who* may spawn is still
  decided by Ariel's own two-key authorization (ADR 0012), so an `operate` token in
  Ariel's hands is as powerful as the most privileged person Ariel will act for.
- **ADR 0008 now partly stands on different ground.** Its NetworkPolicy allow-list
  is defence in depth rather than the only barrier, and the LAN dashboard exception
  it accepted is narrowed by prosperod's own login. The allow-list is kept; removing
  it is a separate decision.
- **Revisit if:** prosperod gains per-request delegation (Ariel acting *as* the
  person rather than as itself), or Ariel needs `admin` for a workspace command.
