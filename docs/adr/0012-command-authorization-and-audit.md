# ADR 0012 · Command authorization: grant scope, what is mutating, and when it is audited

- **Status:** accepted
- **Date:** 2026-09-16
- **Source:** issue #17; [ADR 0009](0009-channel-config.md), [ADR 0008](0008-secrets-deployment-and-network-boundary.md), gonzalo ADR 0022

## Context

[ADR 0009](0009-channel-config.md) fixes the two keys: a command runs at the lower
of the person's role and the channel's ceiling, acts only on workspaces its
channel follows, and is refused in an unconfigured channel. Issue #17 adds that a
workspace-scoped grant applies only to that workspace, and that every mutating
command is audited, allowed or denied.

Three questions were left open, and each decides what a user can do or what the
trail records:

- **Which grants apply to a command that acts on no workspace**, such as a
  fleet-wide status? A person may hold a fleet grant, a workspace grant, both, or
  neither.
- **What makes a command mutating.** `CommandSpec` has a `min_role` but no flag
  saying whether the command changes anything.
- **When the audit entry is written.** Before the command runs, only the decision
  is known; after it runs, the outcome is known too, but a caller might forget to
  write the entry.

## Decision

### Grant scope

- A person's role for a command is the **highest applicable grant**.
- A **fleet grant always applies**.
- A **workspace grant applies only to a command acting on that workspace**. It
  never authorizes another workspace, and never a command that acts on no
  workspace.
- A linked person with no applicable grant is denied (`NoRole`), distinct from an
  unlinked account (`Unlinked`).

### Order of checks

Channel configured → workspace followed → account linked → applicable grant →
effective role reaches `min_role`. The channel checks come first so that a
refusal in a misconfigured channel says so, rather than blaming the person.

### Mutating

A command is **mutating when its `min_role` is above `viewer`**. Read-only
commands are open to viewers by construction; anything that needs `operator` or
`admin` changes the fleet or its configuration.

### Audit

Every mutating command leaves **exactly one** audit entry:

- A **denial is recorded by `authorize` itself**, so no caller can skip it, with
  result `Denied`. An unlinked account is recorded as
  `FleetActor::Unlinked { authenticator, subject }`, so attempts by unknown
  accounts stay visible.
- An **allowed command is recorded by `record_outcome` after it runs**, with
  `Succeeded` or `Failed`, so the trail shows what actually happened.
- The entry carries the action (`command.<name>`), the target workspace (or
  `fleet`), and a reference to the chat message.

Read-only commands are not audited.

## Consequences

- **Positive:** Least privilege by default: a workspace grant cannot leak into
  fleet-wide or neighbouring commands, and a new person or channel can do nothing
  until someone grants it on purpose. Denials cannot go unaudited, and allowed
  entries record outcomes rather than intentions.
- **Negative:** Someone holding only workspace grants cannot run fleet-wide
  read-only commands, even `status`, without a fleet `viewer` grant. "Mutating"
  is inferred from `min_role`, so a future command that changes state but is open
  to viewers would escape the audit. An allowed command whose caller never calls
  `record_outcome` leaves no entry; the router (#20) owns that call.
- **Revisit if:** a read-only command must be audited, or a mutating one opened to
  viewers (add an explicit flag to `CommandSpec`); or people with only workspace
  grants need a workspace-filtered fleet view.
