# Getting Started

Ariel is not yet a working bridge: `arield` starts, reads its credentials and
serves health checks, but does not connect to a chat platform or to prosperod
([Status & Roadmap](./status.md)). This page covers building, testing and running
what exists.

## Prerequisites

- A Rust toolchain with edition 2024 support. The container build uses Rust 1.95.
- For the coverage gate only: `cargo-llvm-cov` and the LLVM tools (see
  `scripts/coverage.sh`).

## Build and test

```sh
git clone https://github.com/caliban-ai/ariel
cd ariel

cargo build --workspace            # arield (with Discord) and ariel
cargo test --workspace             # unit, contract and golden-fixture tests
```

The full gate CI runs:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --all-targets
cargo test --workspace

# a build with no chat backend must also pass
cargo build -p ariel-daemon --no-default-features
cargo clippy -p ariel-daemon --all-targets --no-default-features -- -D warnings
cargo test -p ariel-daemon --no-default-features

scripts/coverage.sh                # 85% line-coverage floor
```

No test needs network access, a Discord token, or a running prosperod: the
prospero client is tested against a local stub server, and the Discord backend
against a stub of Discord's REST API.

## Run `arield`

```sh
cargo run -p ariel-daemon --bin arield
```

With no environment set, it prints the providers compiled in and serves health
checks on `0.0.0.0:8081`:

```text
compiled providers: [discord]
```

```sh
curl http://127.0.0.1:8081/healthz     # ok
```

It stops on Ctrl-C or SIGTERM. See [Configuration](./configuration.md) for the
variables it reads.

## Run the CLI

```sh
cargo run -p ariel-cli --bin ariel -- --version
```

The CLI has no subcommands yet.

## Container

Build the image from the repository root:

```sh
docker build -t ariel:dev .
```

The image runs `arield` as uid 10001 on a slim Debian base, sets
`ARIEL_HEALTH_ADDR=0.0.0.0:8081`, and exposes port 8081. Ariel writes nothing
locally, so it runs with a read-only root filesystem and no volume. Mount
credentials as files and point the `*_TOKEN_FILE` variables at them:

```sh
docker run --rm --read-only -p 8081:8081 \
  -v "$PWD/secrets:/run/secrets/ariel:ro" \
  -e ARIEL_DISCORD_TOKEN_FILE=/run/secrets/ariel/discord-token \
  ariel:dev
```

Released images are to be published as `ghcr.io/caliban-ai/ariel`, tagged by
version and `sha-<commit>`, for `linux/amd64` and `linux/arm64`, when a `v*` tag is
pushed. No release has been tagged yet.

## Try the Discord backend

The Discord backend can already register a command, post, and answer an
interaction in a real guild through a standalone example. See
[Discord Setup](./discord.md).
