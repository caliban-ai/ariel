# ADR 0003 · Ariel stores nothing of its own; its state is gonzalo records

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` (caliban-ai umbrella workspace) §Guiding decisions 4, §Identity, RBAC & audit, §Non-goals

## Context

Ariel authorizes commands per person, not per channel. That needs durable state:
people, the chat and SSO accounts bound to them, role grants, per-channel
configuration and role ceilings, one-time link tokens, and an audit trail of
every mutating action.

The options weighed were:

- **A local database in arield** (SQLite, or Postgres in a cluster). Simple to
  start, but it is a new persistence substrate to back up, migrate, and replicate,
  and the identity model would be trapped in one service on one host.
- **Gonzalo records.** Gonzalo already provides versioned records with optimistic
  concurrency, so concurrent grant edits surface a conflict instead of silently
  losing one; append-only merge for audit; and syncable, shareable storage across
  its substrates.

People and roles are not Ariel-specific: prospero's own API authentication
(caliban-ai/prospero#2) needs the same model, which argues for keeping it where
other services can read it.

## Decision

We will keep **no persistent state inside Ariel**. Identity, role grants, channel
configuration, link tokens, and the audit trail are **gonzalo records**, read and
written through gonzalod. `arield` holds only in-memory, reconstructible state,
such as open event streams and caches.

The record kinds themselves are gonzalo's to define. Their names, merge classes,
token hashing, and rollout order are being settled in gonzalo's ADR for
fleet access-control records (caliban-ai/gonzalo#277), implemented in
caliban-ai/gonzalo#278. Ariel consumes that model; it does not define a parallel
one.

## Consequences

- **Positive:** No new persistence substrate to operate. Versioning, conflict
  surfacing, and audit merge come from gonzalo already tested. The identity model
  is shared by any service that governs human access to the fleet, and `arield`
  can be restarted or replaced without data loss.
- **Negative:** Ariel's identity layer is **blocked** until gonzalo ships the new
  record kinds in a release, and every gonzalod must be upgraded before Ariel
  writes them. Every authorized command costs a round trip to gonzalod, and
  Ariel cannot authorize anything while gonzalod is unreachable.
- **Revisit if:** gonzalo's record model cannot express a chat requirement, the
  gonzalod round trip makes commands miss platform response deadlines even with
  caching, or Ariel needs state that must not leave its own process.
