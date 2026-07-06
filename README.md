# pg-maintainer

A single-threaded PostgreSQL table maintenance tool written in Rust. It runs four sequential maintenance modes against one or more schemas, targeting only the tables that actually need work.

## Why pg-maintainer?

- **No extensions required** — vacuum/analyze/freeze use only standard `pg_catalog` views; bloat detection is statistics-based (`pg_stat_user_tables`), not `pgstattuple` or `pg_repack`. Works on any standard PostgreSQL installation, including managed services where you can't install extensions.
- **Targets only what needs work** — each mode discovers real candidates (never vacuumed, never analyzed, wraparound risk, or bloat above threshold) instead of blindly running maintenance across every table.
- **Safe by default** — active-vacuum detection skips conflicting tables (or terminates them with `--force`); a 10ms `lock_timeout` makes runs fail fast instead of blocking production traffic; `--dry-run` previews every action before anything executes.
- **Size-aware** — `--min-table-size-gb`/`--max-table-size-gb` exclude tiny or oversized tables from any mode.
- **Flexible credentials** — `PG_PASSWORD` env var, `PG_PASSWORD_FILE` (Docker/Kubernetes secrets), `.pgpass`, or CLI flag (with an insecurity warning). No plaintext passwords required in scripts.
- **Container-ready** — ships as a Docker image; all connection config comes from environment variables or mounted secrets, so it drops straight into a Kubernetes `CronJob` or a docker-compose one-off job.
- **Single connection, sequential execution** — no thread pool, no partial state to reconcile; straightforward to reason about and safe to re-run.

## Who Is This For?

- **DBAs and SREs** who want scheduled vacuum/analyze/freeze/bloat maintenance without hand-rolling SQL scripts.
- **Teams on managed PostgreSQL** (RDS, Aurora, Cloud SQL, Supabase, Neon, etc.) where extension-based tools like `pgstattuple` or `pg_repack` aren't installable — pg-maintainer only reads standard catalog views and statistics.
- **Infrastructure/platform engineers** who want a single container or binary to drop into a cron job, Kubernetes `CronJob`, or CI pipeline step.
- **Small teams without a dedicated DBA** who need "find the tables that actually need vacuum/analyze/freeze/bloat cleanup and handle only those" without building that logic themselves.

## Table of Contents

- [Maintenance Modes](#maintenance-modes)
- [Installation](#installation)
- [Usage](#usage)
- [Environment Variables](#environment-variables)
- [Options](#options)
- [Command Line Interface](#command-line-interface)
- [Key Features](#key-features)
- [Integration Testing](#integration-testing)
- [Config File](#config-file)
- [License](#license)

## Maintenance Modes

| # | Operation | Targets |
|---|---|---|
| 1 | `VACUUM` | Tables where neither manual nor autovacuum has ever run |
| 2 | `ANALYZE` | Tables where neither manual nor autoanalyze has ever run |
| 3 | `VACUUM (VERBOSE, FREEZE, INDEX_CLEANUP FALSE)` | Tables whose XID age exceeds the wraparound threshold |
| 4 | `VACUUM (VERBOSE)` | Tables with excessive dead tuples (bloat > threshold, default 80%) |

All four modes run in sequence on a single connection. Select individual modes with `--mode` (default: all four). A table matched by an earlier mode in the same run is not reprocessed by a later mode.

## Installation

### From source

```bash
cargo build --release
# binary at: target/release/pg-maintainer
```

### Docker image

Build and run the container image (debian-slim based, ~113MB):

```bash
docker build -t pg-maintainer:latest .

# Run a maintenance task
docker run --rm \
  -e PG_HOST=db.internal -e PG_PORT=5432 -e PG_DATABASE=mydb \
  -e PG_USER=maintainer -e PG_PASSWORD=secret \
  pg-maintainer:latest --discover-all-schemas --mode vacuum,analyze

# Use a secret file for the password (recommended)
docker run --rm \
  -e PG_HOST=db.internal -e PG_PORT=5432 -e PG_DATABASE=mydb \
  -e PG_USER=maintainer -e PG_PASSWORD_FILE=/run/secrets/pg_password \
  -v pg_secret:/run/secrets/pg_password:ro \
  pg-maintainer:latest --discover-all-schemas
```

For Kubernetes `CronJob` deployments, mount the password secret as a file:

```yaml
spec:
  containers:
  - name: pg-maintainer
    image: pg-maintainer:latest
    env:
    - name: PG_HOST
      value: postgres.default.svc.cluster.local
    - name: PG_PASSWORD_FILE
      value: /run/secrets/pg_password
    volumeMounts:
    - name: pg-secret
      mountPath: /run/secrets
      readOnly: true
    args:
    - --discover-all-schemas
    - --mode
    - vacuum,analyze,freeze
  volumes:
  - name: pg-secret
    secret:
      secretName: pg-password
      items:
      - key: password
        path: pg_password
        mode: 0400
```

## Usage

```bash
# Maintain all user schemas in a database
pg-maintainer -d mydb --discover-all-schemas

# Maintain specific schemas
pg-maintainer -d mydb -s public,analytics

# Maintain a single table
pg-maintainer -d mydb -s public -t users

# Dry run — print commands without executing them
pg-maintainer -d mydb -s public --dry-run

# Run only vacuum and bloat modes
pg-maintainer -d mydb -s public --mode vacuum,bloat

# Detect bloat with custom threshold (70% instead of 80%)
pg-maintainer -d mydb -s public --mode bloat --bloat-threshold-pct 70

# Filter tables by size
pg-maintainer -d mydb -s public --min-table-size-gb 0.5 --max-table-size-gb 10

# SSL connection to a remote server
pg-maintainer -d mydb -s public -H prod-db.company.com --sslmode verify-full --ssl-ca-cert /path/to/ca.pem

# Use a config file
pg-maintainer -C config.toml
```

## Environment Variables

```bash
export PG_HOST=localhost
export PG_PORT=5432
export PG_DATABASE=mydb
export PG_USER=postgres
export PG_PASSWORD=mypassword

# Or read the password from a file (Docker/Kubernetes secrets)
export PG_PASSWORD_FILE=/run/secrets/pg_password

# Or via .pgpass (must be mode 0600)
export PGPASSFILE=/path/to/.pgpass
```

Password resolution order: `--password` (CLI, emits an insecurity warning) → `PG_PASSWORD` → `PG_PASSWORD_FILE` → `.pgpass`/`$PGPASSFILE` → none.

Overall configuration precedence: CLI arguments → TOML config file (`-C`) → environment variables → defaults.

## Options

### Connection
| Flag | Env var | Default |
|---|---|---|
| `-H, --host` | `PG_HOST` | `localhost` |
| `-p, --port` | `PG_PORT` | `5432` |
| `-d, --database` | `PG_DATABASE` | `postgres` |
| `-U, --username` | `PG_USER` | `postgres` |
| `-P, --password` | `PG_PASSWORD` / `PG_PASSWORD_FILE` | — |

### Schema & Table
| Flag | Description |
|---|---|
| `-s, --schema` | Comma-separated schema names |
| `--discover-all-schemas` | Maintain every user schema (excludes system schemas) |
| `-t, --table` | Limit all phases to a single table |

### Maintenance Modes & Tuning
| Flag | Description |
|---|---|
| `-f, --dry-run` | Print commands without executing them |
| `--mode` | Comma-separated modes to run: `vacuum`, `analyze`, `freeze`, `bloat` (default: all four) |
| `--force` | Terminate active vacuums before starting (useful with `--mode freeze`) |
| `--bloat-threshold-pct` | Bloat percentage threshold for Phase 4 (default: `80.0`) |
| `--min-table-size-gb` | Exclude tables smaller than this; all modes (default: `0`, no floor) |
| `--max-table-size-gb` | Exclude tables larger than this; all modes (default: none, no ceiling) |
| `--wraparound-min-age` | XID age threshold for Phase 3 (default: `200000000`) |
| `--wraparound-pct` | Wraparound threshold as % of `autovacuum_freeze_max_age`; overrides `--wraparound-min-age` |
| `-w, --maintenance-work-mem-gb` | Session `maintenance_work_mem` in GB (default: `1`, max: `32`) |

### SSL
| Flag | Description |
|---|---|
| `--sslmode` | `disable` \| `require` \| `verify-ca` \| `verify-full` (default: `disable`) |
| `--ssl-ca-cert` | Path to CA certificate `.pem` |
| `--ssl-client-cert` | Path to client certificate `.pem` |
| `--ssl-client-key` | Path to client private key `.pem` |

### Logging
| Flag | Description |
|---|---|
| `-l, --log-file` | Log file path (default: `maintainer.log`) |
| `--log-format` | `text` \| `json` (default: `text`) |
| `--silence-mode` | Suppress terminal output; logs still written to file |

### Config
| Flag | Description |
|---|---|
| `-C, --config` | Path to TOML config file; CLI arguments take precedence |

## Command Line Interface

```
pg-maintainer — PostgreSQL table maintenance: vacuum, analyze, and anti-wraparound freeze

Usage: pg-maintainer [OPTIONS]

Options:
  -H, --host <HOST>
          PostgreSQL host (or PG_HOST env var)
  -p, --port <PORT>
          PostgreSQL port (or PG_PORT env var)
  -d, --database <DATABASE>
          Database name (or PG_DATABASE env var)
  -U, --username <USERNAME>
          PostgreSQL username (or PG_USER env var)
  -P, --password <PASSWORD>
          Password. INSECURE: prefer PG_PASSWORD env var.
  -s, --schema <SCHEMA>
          Comma-separated schema names. Mutually exclusive with --discover-all-schemas.
      --discover-all-schemas
          Discover and maintain all user schemas (excludes system schemas)
  -t, --table <TABLE>
          Limit maintenance to a single table name
  -f, --dry-run
          Show what would be done without executing any maintenance commands
      --mode <MODE>
          Modes to run: vacuum, analyze, freeze, bloat
      --force
          Terminate active vacuum/autovacuum on each table before maintaining it. Without --force, tables with an active vacuum are skipped instead
      --bloat-threshold-pct <BLOAT_THRESHOLD_PCT>
          Bloat threshold percentage (default: 80.0). Tables with dead tuple ratio exceeding this percentage are considered bloat candidates [default: 80]
      --min-table-size-gb <GB>
          Minimum table size in GB (default: 0, no floor)
      --max-table-size-gb <GB>
          Maximum table size in GB (default: none, no ceiling)
      --wraparound-min-age <WRAPAROUND_MIN_AGE>
          Minimum XID age threshold for wraparound candidates (default: 200000000) [default: 200000000]
      --wraparound-pct <PCT>
          Wraparound threshold as % of autovacuum_freeze_max_age (0-100). Overrides --wraparound-min-age.
  -w, --maintenance-work-mem-gb <MAINTENANCE_WORK_MEM_GB>
          maintenance_work_mem in GB for this session (default: 1, max: 32) [default: 1]
      --sslmode <SSLMODE>
          [default: disable]
      --ssl-ca-cert <SSL_CA_CERT>
          Path to CA certificate (.pem) for SSL
      --ssl-client-cert <SSL_CLIENT_CERT>
          Path to client certificate (.pem). Requires --ssl-client-key.
      --ssl-client-key <SSL_CLIENT_KEY>
          Path to client private key (.pem). Requires --ssl-client-cert.
  -l, --log-file <LOG_FILE>
          [default: maintainer.log]
      --log-format <LOG_FORMAT>
          [default: text]
      --silence-mode
          Suppress terminal output; all logs still go to the log file
  -C, --config <FILE>
          Path to a TOML configuration file. CLI arguments take precedence
  -h, --help
          Print help
  -V, --version
          Print version
```

## Key Features

- **Selective modes**: run any combination of `vacuum`, `analyze`, `freeze`, `bloat` via `--mode`; omit it to run all four
- **Cross-mode dedup**: a table already handled by an earlier mode in the same run is skipped by later modes instead of being reprocessed
- **Statistics-based bloat detection**: dead-tuple ratio from `pg_stat_user_tables`, no extension or extra table scan required
- **Size filtering**: `--min-table-size-gb`/`--max-table-size-gb` apply across all four modes
- **Active-vacuum awareness**: tables with a conflicting VACUUM/autovacuum in progress are skipped, or the conflicting backend is terminated with `--force`
- **Fast-fail locking**: 10ms `lock_timeout` for the session so runs never block indefinitely behind another process's lock
- **Wraparound tuning**: flag candidates by absolute XID age (`--wraparound-min-age`) or by percentage of `autovacuum_freeze_max_age` (`--wraparound-pct`)
- **SSL/TLS**: `disable`/`require`/`verify-ca`/`verify-full`, with custom CA and mutual TLS support
- **Multiple credential sources**: `PG_PASSWORD`, `PG_PASSWORD_FILE` (Docker/Kubernetes secrets), `.pgpass`/`$PGPASSFILE`, or CLI flag
- **Config file**: TOML configuration with env-var interpolation (`password = "${PG_PASSWORD}"`) and CLI override support
- **Structured logging**: text or JSON log format, optional silence mode, buffered file + stdout output
- **Dry run**: preview every VACUUM/ANALYZE candidate and command before anything executes

## Integration Testing

### Local Docker + pgbench test

Run the integration test suite with pgbench workload:

```bash
# Prerequisites: Docker, pgbench, Rust toolchain
./scripts/docker-integration-test.sh

# With custom pgbench scale and duration
PGBENCH_SCALE=50 PGBENCH_DURATION=60 ./scripts/docker-integration-test.sh

# Build and test release binary
RELEASE=1 ./scripts/docker-integration-test.sh
```

This script:
1. Spins up a PostgreSQL container with autovacuum disabled
2. Loads the test fixture schema (`schema/setup_test_schema.sql`)
3. Initializes pgbench at the specified scale
4. Runs a pgbench workload to generate realistic table churn
5. Tests pg-maintainer in each mode (vacuum, analyze, freeze, bloat)
6. Validates that appropriate tables were identified as candidates

Logs are saved to `logs/` for review.

### Manual fixture testing

To test against the example schema without pgbench:

```bash
# Start the test container
docker compose -f docker-compose.test.yml up -d

# Load the fixture
docker compose -f docker-compose.test.yml exec -T postgres \
  psql -U pgm_test -d pgm_test -f schema/setup_test_schema.sql

# Connect and test pg-maintainer
export PG_HOST=localhost PG_PORT=5432 PG_DATABASE=pgm_test \
       PG_USER=pgm_test PG_PASSWORD=pgm_test

cargo run -- --discover-all-schemas --mode bloat --dry-run

# Cleanup
docker compose -f docker-compose.test.yml down -v
```

## Config File

Copy `config.example.toml` and adjust:

```toml
host     = "localhost"
database = "mydb"
username = "postgres"
password = "${PG_PASSWORD}"   # env-var interpolation supported

discover-all-schemas = true
dry-run = false
mode = "vacuum,analyze,freeze,bloat"   # default when omitted: all four
maintenance-work-mem-gb = 2
```

See `config.example.toml` in the repository for the complete reference, including bloat, size-filter, and wraparound settings.

## License

MIT — see [LICENSE](LICENSE).
