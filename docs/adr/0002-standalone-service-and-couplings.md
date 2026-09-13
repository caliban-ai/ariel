# ADR 0002 · Standalone service: through prospero, with gonzalo, to caliban only via prospero

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` (caliban-ai umbrella workspace) §Guiding decisions 1, §Architecture, §Non-goals

## Context

The caliban-ai stack has no human surface where teams actually work: chat. We
want fleet notifications, commands, approvals, and conversation to reach
Discord, Slack, and Teams.

Chat integration drags in concerns none of the existing services have: platform
SDKs with large TLS and websocket trees, OAuth, request signature verification,
and hard platform rate limits. The options weighed were:

- **Bolt chat onto prospero.** Prospero already serves fleet events, but every
  prosperod would then carry chat SDKs and their attack surface, and prospero's
  release cadence would be tied to three chat platforms' API churn.
- **Talk to caliband directly.** This skips a hop but duplicates prospero's
  fleet discovery and supervision, and creates a second control path that can
  disagree with prospero about the fleet.
- **A standalone bridge.** A fourth service that owns the chat boundary and
  reaches the rest of the stack only through their public interfaces.

The stack already keeps one concern per repository, and prospero keeps the same
wire-format discipline toward caliban that this decision asks of Ariel.

## Decision

We will build Ariel as a **standalone service**, a sibling to prospero and
gonzalo, in its own repository and cargo workspace, shipping an `arield` daemon
and an `ariel` operator CLI. Its couplings to the stack are fixed:

- **Through prospero.** All fleet control and fleet events go over prospero's
  public HTTP and SSE API. Ariel takes **no dependency on prospero crates**; the
  wire format is the contract.
- **With gonzalo.** State lives in gonzalo records (see
  [ADR 0003](0003-no-state-of-its-own.md)).
- **To caliban only through prospero.** Ariel never talks to caliband directly.

Out of scope: a web UI (prospero has a dashboard) and replacing the prospero CLI
or dashboard. Ariel is a complementary surface.

## Consequences

- **Positive:** Chat SDKs, secrets, and rate limiting stay behind one boundary
  that can be deployed, scaled, and upgraded on its own. A prosperod never loads
  chat code. There is exactly one fleet control path, so chat and the dashboard
  cannot disagree about the fleet.
- **Negative:** Another service to deploy and operate. Without prospero crates,
  Ariel must obtain prospero's wire types another way (mirrored types pinned by
  golden fixtures, or a pinned git dependency), decided in #12. Any capability
  prospero's API lacks — for example a fleet-wide event feed, or the
  `PermissionRequested` event approvals need — must be built upstream first or
  worked around.
- **Revisit if:** prospero's public API cannot express what chat needs without
  Ariel reaching into prospero internals, or operating a separate service proves
  costlier than the isolation is worth.
