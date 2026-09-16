# Architecture Decision Records

This directory records the architecturally significant decisions made on
**ariel**, in [MADR-lite](https://adr.github.io/madr/) format — the same
convention used by sibling repos [caliban](https://github.com/caliban-ai/caliban),
[prospero](https://github.com/caliban-ai/prospero), and
[gonzalo](https://github.com/caliban-ai/gonzalo).

An ADR captures a decision that is **architecturally significant**, **costly to
reverse**, or **constrains future work**, together with the context and the
trade-off behind it — so a reader who wasn't there understands *why*. ADRs are
an append-only log: once accepted, an ADR is not rewritten. A decision that
changes gets a *new* ADR that supersedes the old one, and the old one is marked
`superseded` with a link both ways.

## Status legend

- **proposed** — under discussion, not yet adopted
- **accepted** — adopted and in effect
- **rejected** — considered and declined (kept for the record)
- **deprecated** — no longer applies, but not replaced by a specific ADR
- **superseded** — replaced by a later ADR (linked)

## Index

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-record-architecture-decisions.md) | Record architecture decisions | accepted |
| [0002](0002-standalone-service-and-couplings.md) | Standalone service: through prospero, with gonzalo, to caliban only via prospero | accepted |
| [0003](0003-no-state-of-its-own.md) | Ariel stores nothing of its own; its state is gonzalo records | accepted |
| [0004](0004-provider-trait-and-feature-gated-backends.md) | One chat provider trait, feature-gated backend crates | accepted |
| [0005](0005-mirror-prospero-wire-types.md) | Mirror prospero's wire types, pinned by golden fixtures | accepted |
| [0006](0006-chat-provider-trait.md) | The `ChatProvider` trait: one core, capability-gated extras | accepted |
| [0007](0007-notifications-live-messages-and-pacing.md) | Notifications: one live message per agent, paced per channel | accepted |
| [0008](0008-secrets-deployment-and-network-boundary.md) | Secrets, deployment, and the boundary around an unauthenticated prosperod | accepted |
| [0009](0009-channel-config.md) | Channel configuration: what a channel follows, hears, and allows | accepted |
| [0010](0010-discord-library-twilight.md) | Discord backend on twilight | accepted |
| [0011](0011-no-replay-after-a-restart.md) | A restarted daemon does not replay what it missed | accepted |
| [0012](0012-command-authorization-and-audit.md) | Command authorization: grant scope, what is mutating, and when it is audited | accepted |

## Adding a new ADR

1. Copy [`template.md`](template.md) to `NNNN-kebab-title.md` — it carries the
   section skeleton (Status / Date / Context / Decision / Consequences) every
   record uses. The accepted ADRs remain the reference for voice and length.
2. Number it with the next zero-padded integer (`NNNN-kebab-title.md`).
3. Fill in **Status**, **Date** (`YYYY-MM-DD`), and the **Context / Decision /
   Consequences** sections. Keep it to a screen or two.
4. Add a row to the index table above.
5. If it supersedes an earlier ADR, set that ADR's status to `superseded` and
   link both ways.
