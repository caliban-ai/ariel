# ADR 0005 · Mirror prospero's wire types, pinned by golden fixtures

- **Status:** accepted
- **Date:** 2026-09-13

## Context

Ariel reaches the fleet only through prospero's public HTTP and SSE API, with no
dependency on prospero crates ([ADR 0002](0002-standalone-service-and-couplings.md)).
The client still needs typed values for what crosses that API: the fleet
snapshot, agent statuses, fleet events, spawn requests and responses, and SSE gap
frames.

prospero defines all of these in its `prospero-types` crate. That crate is not
published: prospero ships a container image and has no crates.io pipeline. The
options weighed were:

- **A pinned source dependency on `prospero-types`** at a release tag. It is
  cheap, since the crate depends only on serde, but it contradicts ADR 0002 and
  couples Ariel's build to prospero's repository layout and release tags.
- **Mirror the types in `ariel-core`.** Ariel defines only the subset it reads,
  and golden fixtures pin that subset to prospero's wire format.

prospero has no JSON wire fixtures of its own to copy. Its event enum also has no
fallback for unknown variants, so a strict mirror would fail to decode a newer
prosperod's events.

## Decision

We will **mirror** the prospero wire types Ariel uses in `ariel_core::prospero::types`,
and take no source dependency on prospero crates.

- The mirror covers only the fields Ariel reads. Extra fields are ignored on
  decode.
- Every enum Ariel matches on (`AgentStatus`, `WorkspaceHealth`, `OutputStream`,
  `EventKind`) has an `Unknown` fallback, so a newer prosperod that adds a
  variant degrades to "not understood" instead of failing to decode.
- Golden fixtures in `crates/core/tests/fixtures/prospero/` are written from
  prospero's type definitions and cite the prospero version they reflect
  (v0.7.0). Round-trip tests fail when the mirror stops matching them.

## Consequences

- **Positive:** Ariel builds with no access to prospero's source, and ADR 0002
  holds. Unknown fallbacks let Ariel run against a newer prosperod. The fixtures
  make the contract Ariel relies on explicit and reviewable.
- **Negative:** The mirror can drift from prospero silently. The fixtures only
  prove Ariel matches its own reading of the contract, not prospero's current
  output, so a prospero wire change is caught only if someone updates the
  fixtures. Each new field Ariel needs means changing the mirror and its
  fixtures by hand.
- **Revisit if:** prospero publishes a wire-types crate, or prospero ships golden
  fixtures or a schema Ariel can test against directly, or drift causes a real
  decoding bug.
