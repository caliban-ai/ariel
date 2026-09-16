# ADR 0011 · A restarted daemon does not replay what it missed

- **Status:** accepted
- **Date:** 2026-09-15
- **Source:** issue #19 ("decide restart behaviour"); [ADR 0003](0003-no-state-of-its-own.md), [ADR 0007](0007-notifications-live-messages-and-pacing.md)

## Context

Prospero's per-agent stream replays an agent's whole history when a client
connects, and `from` is the only cursor. A daemon that reconnects from `0` after
a restart would re-post every event it had already delivered.

Ariel keeps no state of its own ([ADR 0003](0003-no-state-of-its-own.md)), and
gonzalo's access-control record kinds (gonzalo ADR 0022) have nothing to hold a
stream position. The options were:

- **Persist a cursor.** Add a record kind for the last delivered `seq` per agent,
  or per daemon. It would let Ariel resume exactly, at the cost of a new record
  kind in gonzalo, a write on every notification, and a cursor that is wrong
  whenever two daemons run at once.
- **Notify only what happens after startup.** No new state anywhere, at the cost
  of a gap: an agent that ended while the daemon was down is never announced.

[ADR 0007](0007-notifications-live-messages-and-pacing.md) already narrows the
blast radius. Message refs are not persisted, so a restart cannot edit the live
messages the previous process posted, and the `FleetWatcher` skips agents already
terminal on its first poll.

## Decision

**A restarted `arield` starts from the fleet as it is, and replays nothing.**

- Agents already terminal at the first fleet poll are history: no message.
- Each agent still running gets a **fresh live message**. The message the
  previous process posted is left as it was, showing the last state it saw.
- Agents that started **and** ended entirely during the downtime are never
  notified. This is the accepted gap.
- No cursor is stored, in gonzalo or anywhere else.

## Consequences

- **Positive:** No new record kind, no write per notification, and no cursor to
  be wrong when two daemons overlap during a rolling restart. Restart behaviour
  is a consequence of ADR 0003 rather than a mechanism of its own, and a restart
  costs at most one fresh message per running agent.
- **Negative:** A downtime that spans an agent's whole life hides it from chat
  entirely; the fleet dashboard and prospero's own history remain the record. A
  running agent's old live message is never finalized, so a channel can hold a
  stale message that stops updating. An operator watching one channel cannot tell
  a restart gap from a quiet fleet.
- **Revisit if:** restarts become frequent enough that missed terminal states
  matter (a rolling deployment on every chart change, say), or gonzalo gains a
  cursor-shaped record kind for another consumer, in which case resuming becomes
  cheap enough to reconsider.
