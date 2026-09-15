#!/usr/bin/env bash
# Start a local PostgreSQL, seed the demo data on first boot, then hand over to
# whatever command the container was given (an interactive shell by default).
set -euo pipefail

PGDATA=${PGDATA:-/var/lib/postgresql/data}
SANDBOX_DB=${SANDBOX_DB:-demo}
SEED_MARKER="$PGDATA/.sandbox-seeded"
SOCKET_DIR=/var/run/postgresql

log() { printf '\033[36m[sandbox]\033[0m %s\n' "$*"; }

install -d -o postgres -g postgres "$SOCKET_DIR"
install -d -m 700 -o postgres -g postgres "$PGDATA"

if [ ! -s "$PGDATA/PG_VERSION" ]; then
    log "Initialising PostgreSQL cluster in $PGDATA ..."
    gosu postgres initdb --username=postgres --auth=trust --auth-host=trust >/dev/null
fi

# autovacuum is deliberately OFF. It would clean up the seeded dead tuples and
# stamp last_autovacuum/last_autoanalyze on every table, leaving pg-maintainer
# with nothing to find. Keeping it off is what makes the demo reproducible.
log "Starting PostgreSQL (autovacuum disabled so the demo data stays dirty) ..."
gosu postgres pg_ctl -D "$PGDATA" -w -s start -o "\
    -c listen_addresses=localhost \
    -c unix_socket_directories=$SOCKET_DIR \
    -c autovacuum=off \
    -c track_counts=on \
    -c logging_collector=off" >/dev/null

if [ ! -f "$SEED_MARKER" ]; then
    log "Creating and seeding the '$SANDBOX_DB' database (takes a few seconds) ..."
    gosu postgres createdb "$SANDBOX_DB"
    gosu postgres psql -v ON_ERROR_STOP=1 -q -d "$SANDBOX_DB" -f /opt/sandbox/seed.sql
    gosu postgres touch "$SEED_MARKER"
    log "Seed complete."
fi

cat <<BANNER

  pg-maintainer sandbox
  ---------------------
  PostgreSQL $(gosu postgres psql -tAc 'show server_version') is running locally, autovacuum off.
  Database '$SANDBOX_DB' is seeded with tables that trigger each mode:

    fresh_signups      never vacuumed, never analyzed
    bloated_events     ~90% dead tuples          -> bloated
    stale_customers    heavily modified since analyze -> stale-stats
    quiet_archive      clean; should be skipped

  Try:

    pg-maintainer -s public --dry-run
    pg-maintainer -s public
    pg-maintainer -s public --gentle --mode bloated
    pg-maintainer -s public --mode wraparound --wraparound-min-age 1 --dry-run
    pg-maintainer -s public --dsn "postgres://postgres@/demo?host=/var/run/postgresql"
    pg-maintainer --help

  Inspect the database with:   psql
  Re-seed from scratch with:   reseed
  Table stats at a glance:     tablestats

BANNER

exec "$@"
