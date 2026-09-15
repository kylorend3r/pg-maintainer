# Self-contained pg-maintainer sandbox: the tool and a PostgreSQL server in one
# container, pre-seeded with tables that trigger each maintenance mode.
#
# Build from the repository root:
#   docker build -f testing/sandbox/Dockerfile -t pg-maintainer-sandbox .
#
# This is a playground, not a deployment artifact. It runs a database as the same
# process tree as the tool and trusts every local connection. Use the root
# Dockerfile for anything real.

# ── Builder ────────────────────────────────────────────────────────────────────
# Bookworm-based, matching the postgres runtime image below, so the binary links
# against the same glibc.
FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
      perl make pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# ── Runtime ────────────────────────────────────────────────────────────────────
FROM postgres:17-bookworm

# less/vim are not needed, but psql is far more pleasant with a pager present.
RUN apt-get update && apt-get install -y --no-install-recommends \
      less procps \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/pg-maintainer /usr/local/bin/pg-maintainer

COPY testing/sandbox/seed.sql /opt/sandbox/seed.sql
COPY config.example.toml /opt/sandbox/config.example.toml
COPY testing/sandbox/entrypoint.sh /usr/local/bin/sandbox-entrypoint
COPY testing/sandbox/reseed /usr/local/bin/reseed
COPY testing/sandbox/tablestats /usr/local/bin/tablestats
RUN chmod +x /usr/local/bin/sandbox-entrypoint \
             /usr/local/bin/reseed \
             /usr/local/bin/tablestats

# The database the sandbox creates and seeds.
ENV SANDBOX_DB=demo
# Connection defaults, so `pg-maintainer -s public` works with no flags at all.
ENV PGHOST=/var/run/postgresql \
    PGUSER=postgres \
    PGDATABASE=demo \
    PG_HOST=/var/run/postgresql \
    PG_USER=postgres \
    PG_DATABASE=demo

WORKDIR /workspace

ENTRYPOINT ["/usr/local/bin/sandbox-entrypoint"]
CMD ["bash"]
