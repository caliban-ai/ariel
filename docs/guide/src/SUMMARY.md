# Summary

[ariel](./introduction.md)

# Design

- [Architecture & Couplings](./architecture.md)
- [Status & Roadmap](./status.md)

# Guides

- [Getting Started](./getting-started.md)
- [Configuration](./configuration.md)
- [Discord Setup](./discord.md)

# Architecture Decisions

- [ADR Index](./adr/index.md)
<!-- adrs -->
  - [ADR 0001 · Record architecture decisions](./adr/0001-record-architecture-decisions.md)
  - [ADR 0002 · Standalone service: through prospero, with gonzalo, to caliban only via prospero](./adr/0002-standalone-service-and-couplings.md)
  - [ADR 0003 · Ariel stores nothing of its own; its state is gonzalo records](./adr/0003-no-state-of-its-own.md)
  - [ADR 0004 · One chat provider trait, feature-gated backend crates](./adr/0004-provider-trait-and-feature-gated-backends.md)
  - [ADR 0005 · Mirror prospero's wire types, pinned by golden fixtures](./adr/0005-mirror-prospero-wire-types.md)
  - [ADR 0006 · The `ChatProvider` trait: one core, capability-gated extras](./adr/0006-chat-provider-trait.md)
  - [ADR 0007 · Notifications: one live message per agent, paced per channel](./adr/0007-notifications-live-messages-and-pacing.md)
  - [ADR 0008 · Secrets, deployment, and the boundary around an unauthenticated prosperod](./adr/0008-secrets-deployment-and-network-boundary.md)
  - [ADR 0009 · Channel configuration: what a channel follows, hears, and allows](./adr/0009-channel-config.md)
  - [ADR 0010 · Discord backend on twilight](./adr/0010-discord-library-twilight.md)
