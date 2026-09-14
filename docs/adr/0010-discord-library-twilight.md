# ADR 0010 · Discord backend on twilight

- **Status:** accepted
- **Date:** 2026-09-13
- **Source:** issue #14

## Context

[ADR 0004](0004-provider-trait-and-feature-gated-backends.md) puts each chat
platform in its own feature-gated backend crate, and
[ADR 0006](0006-chat-provider-trait.md) keeps backends thin: they map the
provider-neutral trait onto a platform, own its transport and response
deadlines, and are tested against the shared provider contract with a stub
platform rather than a live one. [ADR 0008](0008-secrets-deployment-and-network-boundary.md)
has Ariel receive Discord events over the Gateway, an outbound WebSocket, so
the backend needs a Gateway client as well as the REST API.

Two maintained Rust libraries cover Discord's Gateway and REST API. As of
2026-09-13:

- **serenity 0.12.5** (released 2025-12-20) is a batteries-included framework
  with its own event handler, cache and helpers. Its changelog calls 0.12.5 the
  last planned 0.12 release, and a `next` branch is in development, so a major
  version migration is likely soon. The framework owns the event loop.
- **twilight 0.17.1** (released 2025-12) is split into separate crates for the
  Gateway (`twilight-gateway`), REST API (`twilight-http`) and data model
  (`twilight-model`), with optional builders (`twilight-util`). It requires Rust
  1.89. The application owns the event loop and composes the pieces it needs.

## Decision

We will build `ariel-discord` on **twilight**: `twilight-gateway` for the Gateway
connection, `twilight-http` for REST calls and interaction responses, and
`twilight-model` for payloads, adding `twilight-util` only for its builders.

- Only `ariel-discord` depends on twilight. No twilight type appears in
  `ariel-core`'s public API; the backend converts at its boundary.
- The backend owns its event loop: it reads Gateway events, turns interactions
  into `Inbound` events with a responder, and meets Discord's three-second
  interaction deadline by deferring on the core's behalf (ADR 0006).
- **TLS.** twilight enables no rustls crypto provider, and building a client
  without one panics. `ariel-discord` pins rustls 0.23 with the `ring` provider
  and installs it before building any client, which also keeps aws-lc's C
  build out of the dependency tree.
- **Testing.** Backend behaviour is verified by the shared provider contract
  suite. The REST client is pointed at a local stub of Discord's API over plain
  HTTP, with twilight's rate limiter turned off, and interactions are fed in
  through the same entry point the Gateway loop uses. The Gateway connection
  itself is covered by a documented manual smoke test in a test guild.

## Consequences

- **Positive:** The backend depends only on the Discord pieces it uses, which
  keeps its dependency tree and build small and matches ADR 0004's reasons for
  gating backends. Owning the event loop keeps the backend a thin, explicit
  mapping onto the `ChatProvider` trait, and separate HTTP and Gateway crates are
  easier to stub in tests than a framework's handlers. There is no pending major
  migration of the kind serenity's `next` branch signals.
- **Negative:** More code in the backend: the Gateway event loop, reconnection
  handling at the application level, and interaction response bookkeeping that a
  framework would provide. twilight's lower-level API makes Discord's protocol
  details visible to whoever maintains the backend. twilight's REST client
  waits on Discord's rate-limit buckets and retries 429 responses itself, so
  the backend never returns `ProviderError::RateLimited` as ADR 0006
  anticipates; ADR 0007's proactive `send_budget` pacing is the real guard
  against flooding a channel.
- **Revisit if:** twilight's maintenance stalls, a twilight release makes the
  backend substantially harder to keep thin, or serenity's next major version
  removes the migration risk while offering something the backend needs.
