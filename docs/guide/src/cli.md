# The `ariel` CLI

`ariel` is the operator command. It manages **channel configuration** and mints **link tokens**. Channel configuration is the
record that decides which workspaces a chat channel follows, how much it hears,
and the highest role a command run there can have
([ADR 0009](./adr/0009-channel-config.md)).

## Where the records live

Channel configuration is a gonzalo record
([ADR 0003](./adr/0003-no-state-of-its-own.md)), so every command needs a store:

| Flag | Meaning |
|---|---|
| `--gonzalo-url` (or `ARIEL_GONZALO_URL`) | gonzalod's base URL. |
| `--token-file` (or `ARIEL_GONZALO_TOKEN_FILE`) | A file holding the bearer token for gonzalod. |
| `--store <dir>` | A local gonzalo store directory instead of gonzalod. |

`--store` and `--gonzalo-url` are mutually exclusive.

## Show a channel

```console
$ ariel channel show --provider discord --tenant 123456789 --channel 987654321
discord/123456789/987654321
  follows: caliban, prospero
  notify:  all
  ceiling: viewer
```

A channel with no configuration exits non-zero and says it is not configured.
Ariel ignores such a channel: it receives no notifications, and commands there
are refused.

## Create or change one

`ariel channel set` creates the configuration, or changes only the fields you
name.

```console
$ ariel channel set --provider discord --tenant 123456789 --channel 987654321 \
    --follows caliban,prospero --notify failures --ceiling operator
created discord/123456789/987654321
  follows: caliban, prospero
  notify:  failures
  ceiling: operator
```

| Flag | Values | Notes |
|---|---|---|
| `--follows` | `fleet`, or a comma-separated list of workspace names | **Required when creating a channel**, since a channel that follows nothing would never hear anything. `fleet` follows every workspace, including ones created later. |
| `--notify` | `all`, `terminal`, `failures` | Defaults to `all`. A preset only narrows what a channel hears; it can never add event kinds. |
| `--ceiling` | `viewer`, `operator`, `admin` | Defaults to `viewer`, so a new channel allows read-only commands until you raise it on purpose. |

Changing a field reports what moved:

```console
$ ariel channel set --provider discord --tenant 123456789 --channel 987654321 --ceiling admin
updated discord/123456789/987654321
  follows: caliban, prospero
  notify:  failures
  ceiling: admin
  ceiling: operator -> admin
```

Setting exactly what is already stored reports `unchanged` and writes nothing.

## Concurrent edits

Every change is a read, a patch and an optimistic write. If someone else changed
the channel in between, **nothing is written**: the command exits with status `2`,
says so, and prints the configuration as it now stands, for you to re-apply your
change on top of.

```console
$ ariel channel set --provider discord --tenant 123456789 --channel 987654321 --ceiling admin
ariel: the channel changed since it was read; nothing was written
stored now: discord/123456789/987654321
  follows: caliban
  notify:  all
  ceiling: operator
$ echo $?
2
```

## Audit

Creating or changing a channel appends an audit entry to gonzalo's `fleet-audit`
namespace, naming the actor, the action (`channel.create` or `channel.update`),
the record, and whether it succeeded. A command that changes nothing is not
audited.

## Link a chat account

Before anyone can run a command, Ariel has to know which person a chat account
belongs to. An administrator mints a **one-time link token**, and the person
redeems it in chat.

```console
$ ariel link new --role operator --workspace caliban
Link token (shown once; give it to the person privately):

  3f9c…e21a

It grants operator in workspace caliban, and expires in 24h.
Redeem it in chat with: /ariel link <token>
```

| Flag | Meaning |
|---|---|
| `--role` | `viewer`, `operator` or `admin`. Required. |
| `--workspace` | Grant the role in this workspace only. Without it the grant is fleet-wide. |
| `--person` | Link to an existing person id rather than creating a new person. |
| `--expires-hours` | How long the token stays redeemable. Defaults to 24. |

The person then runs `/ariel link <token>` anywhere in chat. Ariel binds their
account to a person, grants the role, and **replies only to them**, without
repeating the token.

- **The token is shown once.** Only a hash of it is stored; it appears in no
  record or audit entry.
- **It works once.** Two people redeeming the same token at the same moment get
  one success and one "already used".
- **It expires.** An expired token links nothing.
- **Tokens are marked used, never deleted**, so a sync between gonzalo stores
  cannot bring a used token back.
- **An account that is already linked is refused without using up the token**, so
  a mistaken attempt doesn't waste it.
- Minting, and every redemption attempt, succeeded or not, is audited.

Linking writes identity records. Per
[ADR 0008](./adr/0008-secrets-deployment-and-network-boundary.md), run it only
against a gonzalod with authentication on; otherwise any pod could grant itself a
role.
