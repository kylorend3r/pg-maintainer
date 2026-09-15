# Test Fixture Schemas for pg-maintainer

This directory contains two SQL fixtures. `setup_test_schema.sql` is the lean,
single-schema fixture CI loads on every push — do not change what it targets
without also updating `.github/workflows/rust.yml`. `complex_schema.sql` is a
richer, separate fixture for manual verification only; CI never loads it.

## `setup_test_schema.sql` (used by CI)

Creates a test schema (`pgm_test`) with tables that reliably produce candidates
for each of the four original maintenance modes. Re-run this script before each
test pass to reset the database to a known state.

### Tables and Expected Behavior

| Table | Phase 1 (VACUUM) | Phase 2 (ANALYZE) | Phase 3 (FREEZE) | Phase 4 (BLOAT) |
|---|---|---|---|---|
| `never_maintained` | **Yes** | **Yes** | maybe* | no |
| `bloated` | no† | no† | maybe* | **Yes** |
| `tiny` | no† | no† | maybe* | no |
| `padded` | no† | no† | maybe* | no |

Legend:
- **Yes**: Table is a candidate for this mode
- **no**: Table is NOT a candidate (already maintained or doesn't meet threshold)
- **maybe***: Table qualifies on XID age (every table has nonzero age), but phase 3 is impractical to test without aging 200M transactions; see "Testing Phase 3" below
- †: Vacuumed and analyzed after insertion, so not a "never" candidate

### How to Use

**1. Set up the fixture (once per test session)**

```bash
psql -d postgres -f testing/schema/setup_test_schema.sql
```

Or with environment variables:

```bash
PGHOST=localhost PGPORT=5432 PGUSER=postgres PGPASSWORD=postgres \
  psql -d postgres -f testing/schema/setup_test_schema.sql
```

**2. Test each mode**

```bash
cargo run -- --discover-all-schemas --mode never-vacuumed --dry-run
# Expected: pgm_test.never_maintained

cargo run -- --discover-all-schemas --mode never-analyzed --dry-run
# Expected: pgm_test.never_maintained

cargo run -- --discover-all-schemas --mode wraparound --wraparound-min-age 100 --dry-run
# Expected: all four tables (every table has nonzero XID age, so all exceed min-age=100)

cargo run -- --discover-all-schemas --mode bloated --dry-run
# Expected: pgm_test.bloated (>80% dead tuples after the 5-out-of-5 update)
```

**3. Test all modes together**

```bash
cargo run -- --discover-all-schemas --dry-run
```

**4. Test size filtering**

```bash
cargo run -- --discover-all-schemas --min-table-size-gb 0.01 --dry-run
# Expected: only pgm_test.padded in all phases

cargo run -- --discover-all-schemas --max-table-size-gb 0.001 --dry-run
# Expected: never_maintained, bloated, tiny in their respective phases
```

**Cleanup**

```bash
psql -d postgres -c "DROP SCHEMA IF EXISTS pgm_test CASCADE"
```

---

## `complex_schema.sql` (manual verification only, not used by CI)

Adds two non-system schemas, a declaratively partitioned table, cross-schema
foreign keys, and populated data at realistic scale — exercising multi-schema
discovery and partitioned-parent exclusion for the first time in this repo. See
the fixture's own header comment for the full design rationale.

### What it creates

| Schema | Relation | Kind | State | Expected mode |
|---|---|---|---|---|
| `complex_test_reporting` | `categories` | table | vacuumed/analyzed, untouched | none (clean) |
| `complex_test` | `customers` | table | never vacuumed/analyzed | `never-vacuumed`, `never-analyzed` |
| `complex_test` | `orders` | table | vacuumed baseline, ~80% churn | `bloated` |
| `complex_test` | `sessions` | table | vacuumed baseline, heavy insert/update | `stale-stats` |
| `complex_test` | `reference_codes` | table | vacuumed/analyzed, untouched | none (clean) |
| `complex_test` | `events` | **partitioned parent** | — (no storage of its own) | **none, ever** (relkind = 'p') |
| `complex_test` | `events_2024_q1` | partition | never vacuumed/analyzed | `never-vacuumed`, `never-analyzed` |
| `complex_test` | `events_2024_q2` | partition | vacuumed/analyzed, untouched | none (clean) |
| `complex_test` | `events_2024_q3` | partition | vacuumed baseline, ~80% churn | `bloated` |
| `complex_test` | `events_2024_q4` | partition | vacuumed baseline, heavy insert/update | `stale-stats` |
| `complex_test_reporting` | `daily_rollups` | table | analyzed but never vacuumed | `never-vacuumed` only |

Foreign keys: `complex_test.orders.customer_id → complex_test.customers.customer_id`
(same schema), and `complex_test.orders.category_id →
complex_test_reporting.categories.category_id` (cross-schema).

### How to Use

**1. Load it** (loads into whichever database you point at — coexists fine
alongside `pgm_test` schema from the other fixture, since the schema names
don't collide):

```bash
PGPASSWORD=pgm_test psql -h localhost -U pgm_test -d pgm_test \
  -f testing/schema/complex_schema.sql
```

Or, against `testing/docker-compose.test.yml` (which mounts this whole
directory to `/schema` in the container, so the file is already there):

```bash
docker compose -f testing/docker-compose.test.yml up -d
docker compose -f testing/docker-compose.test.yml exec -T postgres \
  psql -U pgm_test -d pgm_test -f /schema/complex_schema.sql
```

**2. Exercise multi-schema discovery for the first time**

```bash
cargo run -- -d pgm_test -s complex_test,complex_test_reporting --dry-run
cargo run -- -d pgm_test --discover-all-schemas --dry-run
```

**3. Confirm partition-parent exclusion**

```bash
cargo run -- -d pgm_test -s complex_test --mode never-vacuumed --dry-run
# Expected: complex_test.customers, complex_test.events_2024_q1.
# complex_test.events (the parent, relkind = 'p') must NOT appear.
```

**4. Confirm stale-stats mode picks up the expected relations:**

```bash
cargo run -- -d pgm_test -s complex_test --mode stale-stats --dry-run
# Expected: complex_test.sessions, complex_test.events_2024_q4
```

**5. Confirm bloat mode:**

```bash
cargo run -- -d pgm_test -s complex_test --mode bloated --dry-run
# Expected: complex_test.orders, complex_test.events_2024_q3 (both at exactly
# 80% dead tuples, crossing the default --bloat-threshold-pct)
```

**Cleanup**

```bash
psql -h localhost -U pgm_test -d pgm_test -c \
  "DROP SCHEMA IF EXISTS complex_test CASCADE; DROP SCHEMA IF EXISTS complex_test_reporting CASCADE;"
```
