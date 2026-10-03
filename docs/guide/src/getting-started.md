# Getting Started

Ariel is a running bridge: `arield` watches prosperod's fleet, notifies the chat
channels configured to follow it, and answers `/ariel` commands
([Status & Roadmap](./status.md)). This page covers building and testing it from
source, running it from the published image, and the three steps that turn a
bare daemon into a bridge somebody can use.

## Prerequisites

To **run** `arield`, nothing but the image and:

- a **prosperod** (prospero v0.8 or newer if its API authentication is on) and a
  **gonzalod 0.7.0 or newer**, the first release carrying the access-control
  record kinds Ariel stores ([Configuration](./configuration.md#gonzalo-compatibility));
- a **Discord bot** in a guild ([Discord Setup](./discord.md)).

To **build** it:

- A Rust toolchain with edition 2024 support. The container build uses Rust 1.95.
- For the coverage gate only: `cargo-llvm-cov` and the LLVM tools (see
  `scripts/coverage.sh`).

## Build and test

```sh
git clone https://github.com/caliban-ai/ariel
cd ariel

cargo build --workspace --exclude ariel-e2e    # arield (with Discord) and ariel
cargo test --workspace --exclude ariel-e2e     # unit, contract and golden-fixture tests
```

`ariel-e2e` is excluded because its dev-dependencies come from prospero's git
repository; CI runs it as a separate job for the same reason. The full gate CI
runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --exclude ariel-e2e --all-targets -- -D warnings
cargo build --workspace --exclude ariel-e2e --all-targets
cargo test --workspace --exclude ariel-e2e

# a build with no chat backend must also pass
cargo build -p ariel-daemon --no-default-features
cargo clippy -p ariel-daemon --all-targets --no-default-features -- -D warnings
cargo test -p ariel-daemon --no-default-features

scripts/coverage.sh                # 85% line-coverage floor

# the end-to-end smoke, in its own job
cargo test -p ariel-e2e
```

No test in the main gate needs network access, a Discord token, or a running
prosperod: the prospero client is tested against a local stub server, and the
Discord backend against a stub of Discord's REST API. The end-to-end smoke test
needs the network only to fetch prospero's crates; once built it runs prosperod's
and gonzalod's own server code in-process over loopback, with prospero's fake
caliban standing in for agents, and needs no model API key.

## Run `arield`

```sh
cargo run -p ariel-daemon --bin arield
```

With no environment set, it logs what it is and serves health checks on
`0.0.0.0:8081`, but bridges nothing:

```text
  INFO arield: arield starting version="0.3.0" providers=["discord"]
  INFO arield: serving health checks addr=0.0.0.0:8081
  WARN ariel_daemon::bridge: ARIEL_PROSPERO_URL and ARIEL_GONZALO_URL are not both set; serving health only
```

```sh
curl http://127.0.0.1:8081/healthz     # ok
```

The log goes to **stderr**, so `kubectl logs` and a local terminal both show it.
`RUST_LOG` sets the level and `ARIEL_LOG_FORMAT=json` switches to one JSON object
per line. `arield` stops on Ctrl-C or SIGTERM. See
[Configuration](./configuration.md) for everything it reads.

To make it an actual bridge, give it a fleet, a record store and a chat provider:

```sh
export ARIEL_PROSPERO_URL=http://127.0.0.1:7878
export ARIEL_GONZALO_URL=http://127.0.0.1:8080
export ARIEL_GONZALO_TOKEN_FILE=./secrets/gonzalo-token
export ARIEL_PROSPERO_TOKEN_FILE=./secrets/prospero-token
export ARIEL_DISCORD_TOKEN_FILE=./secrets/discord-token
export ARIEL_DISCORD_GUILD_ID=123456789
export ARIEL_DISCORD_APPLICATION_ID=987654321

cargo run -p ariel-daemon --bin arield
```

## Container

The released image is `ghcr.io/caliban-ai/ariel`, built for `linux/amd64` and
`linux/arm64` on every `v*` tag and tagged by version, by `sha-<commit>`, and
`latest` for the newest release:

```sh
docker run --rm --read-only -p 8081:8081 \
  -v "$PWD/secrets:/run/secrets/ariel:ro" \
  -e ARIEL_PROSPERO_URL=http://prosperod:7878 \
  -e ARIEL_GONZALO_URL=http://gonzalod:8080 \
  -e ARIEL_DISCORD_TOKEN_FILE=/run/secrets/ariel/discord-token \
  -e ARIEL_GONZALO_TOKEN_FILE=/run/secrets/ariel/gonzalo-token \
  -e ARIEL_PROSPERO_TOKEN_FILE=/run/secrets/ariel/prospero-token \
  -e ARIEL_DISCORD_GUILD_ID=123456789 \
  -e ARIEL_DISCORD_APPLICATION_ID=987654321 \
  ghcr.io/caliban-ai/ariel:0.3.0
```

The image runs `arield` as uid 10001 on a slim Debian base, sets
`ARIEL_HEALTH_ADDR=0.0.0.0:8081`, and exposes port 8081. Ariel writes nothing
locally, so it runs with a read-only root filesystem and no volume; credentials
arrive as mounted files named by the `ARIEL_*_TOKEN_FILE` variables.

To build it yourself from the repository root:

```sh
docker build -t ariel:dev .
```

For Kubernetes, use the `ariel` chart in
[caliban-ai/helm-charts](https://github.com/caliban-ai/helm-charts), which wires
the Secrets, the probe and the Service URLs for you.

## Configure a channel and link yourself

A running `arield` notifies nothing and refuses every command until a channel has
a configuration record and your chat account is linked. Both are one command each
([The `ariel` CLI](./cli.md)):

```sh
# what this channel follows, how much it hears, and the highest role allowed in it
ariel channel set --provider discord --tenant 123456789 --channel 987654321 \
  --follows fleet --notify all --ceiling operator

# a one-time token to redeem in chat with /ariel link
ariel link new --role operator
```

Both read `ARIEL_GONZALO_URL` and `ARIEL_GONZALO_TOKEN_FILE` from the
environment, or take `--gonzalo-url` and `--token-file`; `--store <dir>` points
at a local gonzalo store instead. `--tenant` is the Discord guild ID and
`--channel` the channel ID. From then on,
further channels and invites can be handled from chat with `/ariel configure`
and `/ariel invite` ([Chat Commands](./commands.md)) — the CLI is only needed to
bootstrap the first one.

## Try the Discord backend

[Discord Setup](./discord.md) covers creating the bot and the manual smoke test
that exercises the backend against a real guild.
