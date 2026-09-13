# ADR 0004 · One chat provider trait, feature-gated backend crates

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** `docs/superpowers/specs/2026-07-03-ariel-chat-bridge-design.md` (caliban-ai umbrella workspace) §Guiding decisions 2–3, §Crate layout, §Feature gating

## Context

Ariel targets three chat platforms, built in order: Discord, then Slack, then
Microsoft Teams. Routing, rendering, authorization, and the prospero and gonzalo
clients are the same whichever platform a message arrives on; only the platform
connection differs.

Chat SDKs are heavy. A Discord gateway library, the Slack API, and the Teams Bot
Framework each pull in large TLS, websocket, and HTTP trees. Linking all three
into every build means slower builds, larger binaries, and attack surface for
platforms a deployment never uses.

The stack already has the pattern this calls for: gonzalo keeps each storage
substrate and ticket system in its own crate behind an optional dependency and a
facade feature, and picks which one runs at runtime.

## Decision

We will put chat platforms behind **one provider trait**, `ChatProvider`, defined
in `ariel-core`. `ariel-core` is always built and **never depends on a chat
platform SDK**; router, renderer, auth, and clients live there and talk only to
the trait.

Each platform is its **own backend crate** (`ariel-discord`, later `ariel-slack`
and `ariel-teams`), consumed by `ariel-daemon` as an `optional = true`
dependency behind a feature of the same name, with `default = ["discord"]`.

- Features are **additive, never mutually exclusive**. A build with
  `discord,slack` compiles and can run both.
- The compiled feature set decides **which providers can exist**; configuration
  decides **which one runs**. Selection is never a feature.
- A backend crate is created with its backend, not ahead of it.

This ADR fixes the boundary and the gating. The trait's exact shape, and whether
one trait can honestly model Discord, Slack, and Teams, is decided separately
(#2).

## Consequences

- **Positive:** A Discord-only build physically cannot load Slack or Teams code:
  smaller binaries, faster builds, and deploy-time capability control. Everything
  above the backend is testable against an in-memory provider with no platform.
  A new platform is a new crate, not a change to the core.
- **Negative:** The trait must be designed across platforms whose features
  (buttons, modals, threads, interaction deadlines) differ, so it risks either a
  lowest common denominator or platform leaks. Every feature combination is a
  build to keep green, starting with `--no-default-features`.
- **Revisit if:** #2 finds that one trait cannot model all three platforms
  without leaking platform types into `ariel-core`, or a backend needs logic that
  cannot stay out of the core.
