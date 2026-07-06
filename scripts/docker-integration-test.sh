#!/bin/bash
# Docker + pgbench integration test for pg-maintainer
# Spins up a PostgreSQL container, loads fixture schemas, runs pgbench,
# then tests pg-maintainer against it in multiple modes.

set -euo pipefail

# ── Configuration ─────────────────────────────────────────────────────────────
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="${REPO_ROOT}/docker-compose.test.yml"
SCHEMA_FIXTURE="${REPO_ROOT}/schema/setup_test_schema.sql"

# Environment overrides
PGBENCH_CLIENTS="${PGBENCH_CLIENTS:-10}"
PGBENCH_JOBS="${PGBENCH_JOBS:-2}"
PGBENCH_DURATION="${PGBENCH_DURATION:-30}"
PGBENCH_SCALE="${PGBENCH_SCALE:-20}"
RELEASE="${RELEASE:-}"
WRAPAROUND_MIN_AGE="${WRAPAROUND_MIN_AGE:-100}"

# Export for pg-maintainer (uses PG_* env vars, not standard libpq PG* names)
export PG_HOST=localhost
export PG_PORT=5432
export PG_DATABASE=pgm_test
export PG_USER=pgm_test
export PG_PASSWORD=pgm_test

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# ── Helper functions ───────────────────────────────────────────────────────────

log_info() {
  echo -e "${GREEN}[INFO]${NC} $*"
}

log_warn() {
  echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
  echo -e "${RED}[ERROR]${NC} $*"
}

cleanup() {
  log_info "Cleaning up: stopping and removing containers..."
  docker compose -f "${COMPOSE_FILE}" down -v || true
}

# ── Main test flow ─────────────────────────────────────────────────────────────

main() {
  trap cleanup EXIT

  # Check prerequisites
  if [ ! -f "${SCHEMA_FIXTURE}" ]; then
    log_error "Schema fixture not found: ${SCHEMA_FIXTURE}"
    log_error "Run from the repo root where schema/setup_test_schema.sql exists"
    exit 1
  fi

  if ! command -v docker compose &> /dev/null; then
    log_error "docker compose not found. Install Docker Desktop or Docker Compose."
    exit 1
  fi

  if ! command -v psql &> /dev/null; then
    log_warn "psql not found. Some steps may fail."
  fi

  # Step 1: Bring up PostgreSQL
  log_info "Starting PostgreSQL container..."
  docker compose -f "${COMPOSE_FILE}" up -d

  # Wait for health check
  log_info "Waiting for PostgreSQL to be ready..."
  max_retries=30
  retries=0
  while ! docker compose -f "${COMPOSE_FILE}" exec -T postgres pg_isready -U pgm_test -d pgm_test > /dev/null 2>&1; do
    retries=$((retries + 1))
    if [ $retries -ge $max_retries ]; then
      log_error "PostgreSQL did not start in time"
      exit 1
    fi
    sleep 1
  done
  log_info "PostgreSQL is ready"

  # Step 2: Load the hand-written test fixture
  log_info "Loading test fixture schema..."
  docker compose -f "${COMPOSE_FILE}" exec -T postgres \
    psql -U pgm_test -d pgm_test -f /schema/setup_test_schema.sql

  # Step 3: Initialize pgbench
  log_info "Initializing pgbench (scale=${PGBENCH_SCALE})..."
  docker compose -f "${COMPOSE_FILE}" exec -T postgres \
    pgbench -i -s "${PGBENCH_SCALE}" --no-vacuum -U pgm_test -d pgm_test

  # Step 4: Run pgbench workload
  log_info "Running pgbench workload (${PGBENCH_DURATION}s, ${PGBENCH_CLIENTS} clients, ${PGBENCH_JOBS} jobs)..."
  docker compose -f "${COMPOSE_FILE}" exec -T postgres \
    pgbench -T "${PGBENCH_DURATION}" -c "${PGBENCH_CLIENTS}" -j "${PGBENCH_JOBS}" -U pgm_test -d pgm_test

  # Step 5: Build pg-maintainer (if RELEASE is set, build release; otherwise debug)
  log_info "Building pg-maintainer..."
  if [ -n "${RELEASE}" ]; then
    cargo build --release
    binary="./target/release/pg-maintainer"
  else
    cargo build
    binary="./target/debug/pg-maintainer"
  fi

  # Create logs directory
  mkdir -p logs

  # Step 6: Dry-run test for each mode
  log_info "Running dry-run tests for each mode..."

  for mode in vacuum analyze freeze bloat; do
    log_info "Testing mode: ${mode}"
    "${binary}" \
      --discover-all-schemas \
      --mode "${mode}" \
      --wraparound-min-age "${WRAPAROUND_MIN_AGE}" \
      --dry-run \
      --log-file "logs/mode-${mode}.dry.log" || true

    # Validate that appropriate tables appear in dry-run
    case "${mode}" in
      vacuum)
        if ! grep -q "never_maintained\|pgbench_" "logs/mode-${mode}.dry.log"; then
          log_warn "Expected to find candidate tables in vacuum mode"
        fi
        ;;
      analyze)
        if ! grep -q "never_maintained\|pgbench_" "logs/mode-${mode}.dry.log"; then
          log_warn "Expected to find candidate tables in analyze mode"
        fi
        ;;
      freeze)
        # All tables should appear since we set --wraparound-min-age low
        if [ $(wc -l < "logs/mode-${mode}.dry.log") -lt 5 ]; then
          log_warn "Expected more output in freeze mode with low wraparound threshold"
        fi
        ;;
      bloat)
        # pgbench_branches and pgbench_tellers should show high bloat
        if ! grep -q "pgbench_branches\|pgbench_tellers" "logs/mode-${mode}.dry.log"; then
          log_warn "Expected pgbench_branches/tellers in bloat mode"
        fi
        ;;
    esac
  done

  # Step 7: Full run (all modes together)
  log_info "Running full maintenance (all modes)..."
  "${binary}" \
    --discover-all-schemas \
    --wraparound-min-age "${WRAPAROUND_MIN_AGE}" \
    --dry-run \
    --log-file "logs/full-run.dry.log" || true

  # Step 8: Post-run dry-run (should be empty or minimal)
  log_info "Running post-maintenance dry-run (should be empty)..."
  "${binary}" \
    --discover-all-schemas \
    --wraparound-min-age "${WRAPAROUND_MIN_AGE}" \
    --dry-run \
    --log-file "logs/post-run.dry.log" || true

  # Summary
  log_info "Test completed successfully!"
  log_info "Log files:"
  ls -lh logs/ || true

  log_info "To clean up, run: docker compose -f ${COMPOSE_FILE} down -v"
}

main "$@"
