# ADR 0001 · Record architecture decisions

- **Status:** accepted
- **Date:** 2026-09-13

## Context

Ariel began as a design spec
(`docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` in the
caliban-ai umbrella workspace), and several of its load-bearing decisions were
settled there before any code existed. That spec lives outside this repository
and is still a draft with open questions, so the settled decisions are easy to
lose among the unsettled ones and invisible to someone reading the code. The
sibling repos caliban, prospero, and gonzalo each keep an ADR log.

## Decision

We will keep an Architecture Decision Record log under `docs/adr/`, in
**MADR-lite** format, matching the sibling repos. Each architecturally
significant, hard-to-reverse, or future-constraining decision gets one
append-only record with **Context**, **Decision**, and **Consequences**.
Superseded decisions are marked and linked, never deleted.

ADRs 0002–0004 are **retrospective**: they record decisions the design spec
already settled. Decisions still open in the spec (the provider trait's exact
shape, channel mapping, rate limits, secrets and deployment) get their own ADRs
as they are made.

## Consequences

- **Positive:** One durable home, beside the code, for "why"; the same format a
  reader already knows from the sibling repos; settled decisions are separated
  from the spec's open questions.
- **Negative:** An ongoing discipline cost — a significant decision now means
  writing an ADR, not just code. Retrospective ADRs risk rationalizing after the
  fact rather than capturing the live trade-off.
- **Revisit if:** the MADR-lite format diverges from the cross-sibling standard,
  or the log proves too heavyweight for the project's cadence.
