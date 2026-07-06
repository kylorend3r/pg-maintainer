# Multi-stage Dockerfile for pg-maintainer
# Build: Rust with gnu target (musl had issues with vendored OpenSSL)
# Runtime: debian:bookworm-slim for compatibility

# ── Builder ────────────────────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
      perl make pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Build with gnu target (default)
RUN cargo build --release

# ── Runtime ────────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder \
  /build/target/release/pg-maintainer /usr/local/bin/pg-maintainer

ENTRYPOINT ["/usr/local/bin/pg-maintainer"]
