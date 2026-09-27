# pg-maintainer sandbox

A single container holding both pg-maintainer and its own PostgreSQL server,
pre-seeded with tables that trigger each maintenance mode. Nothing else to install,
no database to point at, nothing to clean up afterwards.

This is a playground, not a deployment artifact. It runs a database in the same
container as the tool and trusts every local connection. Use the repository root
`Dockerfile` for anything real.

## Build and run

```bash
# from the repository root, not from testing/sandbox/
docker build -f testing/sandbox/Dockerfile -t pg-maintainer-sandbox .
docker run --rm -it pg-maintainer-sandbox
```

That drops you at a shell with the server already running and seeded. Connection
details are preset, so only `--mode` is needed (every mode is opt-in — none run
unless selected):

```bash
pg-maintainer -s public --mode never-vacuumed,never-analyzed,prevent-wraparound,prevent-bloat --dry-run     # see what it would do
pg-maintainer -s public --mode never-vacuumed,never-analyzed,prevent-wraparound,prevent-bloat               # actually do it
```

Add `-v` nothing and mount nothing: the container is self-contained and throws its
data away on exit. Drop `--rm` if you want the state to survive between runs.

## What is inside

| Table | State | Mode that picks it up |
|---|---|---|
| `fresh_signups` | Never vacuumed, never analyzed | `never-vacuumed`, `never-analyzed` |
| `bloated_events` | ~90% dead tuples (above the 80% bloat threshold) | `prevent-bloat` |
| `needs_vacuum_orders` | ~25% dead tuples (below bloat threshold, above the real autovacuum formula) | `needs-vacuum` |
| `quiet_archive` | Clean, fresh statistics | none; should be skipped |

Every mode is opt-in, so `--mode` is required. Running with
`--mode never-vacuumed,never-analyzed,prevent-wraparound,prevent-bloat` touches
`fresh_signups` and `bloated_events` and leaves `needs_vacuum_orders` and
`quiet_archive` alone. Add `,needs-vacuum` to also pick up `needs_vacuum_orders`.

A vacuum-horizon (xmin) diagnostic runs once before phase 1 by default; pass
`--skip-xmin-horizon-check` to disable it.

**Autovacuum is off.** That is deliberate. Left on, the background worker would
clean up the seeded dead tuples and stamp every table as recently vacuumed, leaving
the tool with nothing to find.

## Helper commands

| Command | Purpose |
|---|---|
| `tablestats` | The counters the discovery queries read: live and dead rows, bloat percentage, modifications since analyze, XID age |
| `reseed` | Rebuild the demo tables, undoing whatever your last run did |
| `psql` | A shell on the `demo` database, no arguments needed |

A typical loop is `tablestats`, then a run, then `tablestats` again to see what
changed, then `reseed` to start over.

## Things worth trying

```bash
# Throttled, so maintenance yields to production traffic
pg-maintainer -s public --gentle --mode prevent-bloat

# The needs-vacuum mode: dead tuples exceed the real autovacuum
# threshold formula, but not the 80% prevent-bloat cutoff
pg-maintainer -s public --mode needs-vacuum --dry-run

# One connection string instead of five flags
pg-maintainer -s public --mode prevent-bloat --dsn "postgres://postgres@/demo?host=/var/run/postgresql"

# Wraparound needs a lowered threshold here: freezing 200 million transactions
# to produce a genuine candidate is not practical in a demo
pg-maintainer -s public --mode prevent-wraparound --wraparound-min-age 1 --dry-run

# Skip the automatic vacuum-horizon (xmin) pre-flight check
pg-maintainer -s public --mode prevent-bloat --skip-xmin-horizon-check --dry-run

# JSON logs, for piping somewhere
pg-maintainer -s public --mode prevent-bloat --log-format json --dry-run

# The tool writes a run history to its own schema
psql -c 'SELECT operation, mode, status, dead_tuples_removed, duration_ms
         FROM maintainer_logbook.maintenance_logbook ORDER BY logged_at'
```

The example config file is at `/opt/sandbox/config.example.toml` if you want to try
`-C`.
