# syntax=docker/dockerfile:1

# ---- builder ----
FROM rust:1.95-bookworm AS builder
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release -p ariel-daemon --bin arield

# ---- runtime ----
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
RUN useradd --uid 10001 --no-create-home --shell /usr/sbin/nologin app
COPY --from=builder /src/target/release/arield /usr/local/bin/arield
# Ariel stores nothing locally (ADR 0003), so the image runs with a read-only
# root filesystem and needs no volume. Credentials arrive as mounted files named
# by ARIEL_*_TOKEN_FILE (ADR 0008).
ENV ARIEL_HEALTH_ADDR=0.0.0.0:8081
USER 10001
EXPOSE 8081
ENTRYPOINT ["arield"]
