# Testing Infrastructure

Docker-based fixtures, harnesses, and a sandbox demo for pg-maintainer — none of
this is part of the shipped crate/image. This is the index; each piece has its
own README with the exact commands.

| Piece | What it is | Docs |
|---|---|---|
| `docker-compose.test.yml` + `docker-integration-test.sh` | Spins up a throwaway `postgres:latest`, loads `schema/setup_test_schema.sql`, runs a pgbench load, then dry-runs pg-maintainer in every mode. Not used by CI (see below). | this file, "Docker-compose harness" |
| `sandbox/` | A self-contained demo image bundling the tool and its own PostgreSQL server, pre-seeded, for interactive exploration. Explicitly "not a deployment artifact." | [`sandbox/README.md`](sandbox/README.md) |
| `schema/` | SQL fixtures: `setup_test_schema.sql` (lean, single-schema — what CI loads) and `complex_schema.sql` (multi-schema, partitioned table, foreign keys — manual verification only). | [`schema/README.md`](schema/README.md) |

## Docker-compose harness

```bash
docker compose -f testing/docker-compose.test.yml up -d
./testing/docker-integration-test.sh
docker compose -f testing/docker-compose.test.yml down -v
```

Credentials/port match `tests/logging_and_dry_run.rs`'s `#[ignore]` DB-integration
tests: `host=127.0.0.1 port=5432 user=pgm_test password=pgm_test dbname=pgm_test`.

## CI's integration tests

CI does **not** use this compose file — `.github/workflows/rust.yml`'s
`integration_tests` job defines its own inline `postgres:16` GitHub Actions
service container and loads `testing/schema/setup_test_schema.sql` directly with
`psql`. See that workflow file for the exact steps.

## Sandbox demo

```bash
docker build -f testing/sandbox/Dockerfile -t pg-maintainer-sandbox .
docker run --rm -it pg-maintainer-sandbox
```

See [`sandbox/README.md`](sandbox/README.md) for what's inside.
