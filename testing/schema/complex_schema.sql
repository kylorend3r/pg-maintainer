-- Complex test fixture for pg-maintainer: multi-schema, partitioned-table, and
-- foreign-key scenario for MANUAL verification testing.
--
-- This is deliberately separate from setup_test_schema.sql, which CI loads as-is
-- on every push. Nothing here is wired into CI -- it exists to exercise code
-- paths the lean fixture cannot:
--   * multiple non-system schemas in one database (--schema a,b and
--     --discover-all-schemas), never previously exercised against a real
--     database in this repo;
--   * a declaratively partitioned table (parent + partitions in different
--     maintenance states), confirming the partitioned-parent exclusion
--     (relkind != 'p') in queries.rs actually holds up end to end, and that
--     individual partitions are discovered/maintained exactly like any other
--     table, with no special-casing;
--   * a foreign-key relationship, including one that crosses schemas.
--
-- Idempotent: safe to re-run. As in setup_test_schema.sql, every VACUUM/ANALYZE
-- call is explicit -- autovacuum is off in every environment this is loaded
-- into (testing/docker-compose.test.yml, the CI service container, and
-- testing/sandbox/), so nothing here relies on the background worker.
--
-- The \connect calls are load-bearing, not decoration (same rationale as
-- testing/sandbox/seed.sql). pg_stat_user_tables' n_live_tup/n_dead_tup/
-- n_mod_since_analyze are backend-local pending counters, flushed to shared
-- memory only on session end or reconnect. VACUUM/ANALYZE *set* those counters
-- outright, but a backend's own still-pending counters for a table (e.g. from
-- the INSERT that preceded the VACUUM) get added on top whenever they finally
-- flush -- verified empirically: without a \connect between each state-changing
-- step, dead-tuple ratios read back roughly half of what they actually are.
-- The rule applied throughout below: \connect immediately after every
-- VACUUM/ANALYZE, and immediately after every DML block whose count needs to
-- be accurate before the next VACUUM/ANALYZE on that same table.

-- ── Phase 0: idempotent teardown ─────────────────────────────────────────────

DROP SCHEMA IF EXISTS complex_test CASCADE;
DROP SCHEMA IF EXISTS complex_test_reporting CASCADE;

-- ── Phase 1: reporting schema + categories (small lookup, referenced
--    cross-schema by complex_test.orders below) ────────────────────────────

CREATE SCHEMA complex_test_reporting;

CREATE TABLE complex_test_reporting.categories (
    category_id   int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    category_name text NOT NULL UNIQUE,
    is_active     boolean NOT NULL DEFAULT true
);

INSERT INTO complex_test_reporting.categories (category_name) VALUES
    ('electronics'), ('books'), ('home-garden'), ('sporting-goods'),
    ('toys'), ('grocery'), ('apparel'), ('automotive'),
    ('office-supplies'), ('beauty'), ('pet-supplies'), ('outdoor');

VACUUM ANALYZE complex_test_reporting.categories;
\connect
-- Deliberately small and static: real lookup tables don't grow into the
-- tens-of-thousands-of-rows range the rest of this fixture targets. Left
-- clean after this baseline -- should never be a candidate in any mode.

-- ── Phase 2: primary schema + customers (never vacuumed, never analyzed) ────

CREATE SCHEMA complex_test;

CREATE TABLE complex_test.customers (
    customer_id  bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    full_name    text NOT NULL,
    email        text NOT NULL,
    signed_up_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO complex_test.customers (full_name, email, signed_up_at)
SELECT 'Customer ' || g,
       'customer' || g || '@example.test',
       now() - (g || ' minutes')::interval
FROM generate_series(1, 60000) g;
\connect
-- deliberately: no VACUUM, no ANALYZE run against this table.

-- ── Phase 3: orders -- FK to customers (same schema) and to
--    complex_test_reporting.categories (cross-schema); vacuumed/analyzed once,
--    then heavily deleted from with no re-vacuum -> bloated ────────────────

CREATE TABLE complex_test.orders (
    order_id     bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id  bigint NOT NULL REFERENCES complex_test.customers(customer_id),
    category_id  int NOT NULL REFERENCES complex_test_reporting.categories(category_id),
    placed_at    timestamptz NOT NULL DEFAULT now(),
    amount_cents bigint NOT NULL,
    notes        text
);

INSERT INTO complex_test.orders (customer_id, category_id, placed_at, amount_cents, notes)
SELECT 1 + (g % 60000),
       1 + (g % 12),
       now() - (g || ' seconds')::interval,
       (g % 500) * 100,
       repeat('n', 80)
FROM generate_series(1, 100000) g;

VACUUM ANALYZE complex_test.orders;
\connect

DELETE FROM complex_test.orders
WHERE order_id % 5 != 0;
\connect
ANALYZE complex_test.orders;
\connect
-- DELETE, not UPDATE: bulk UPDATEs leave n_live_tup badly over-counted even
-- with correct \connect flushing -- confirmed empirically, not a guess.
-- DELETE's dead-tuple counting doesn't have that failure mode, and with the
-- ANALYZE + \connect above this reaches exactly 80% dead-tuple ratio,
-- crossing the default --bloat-threshold-pct. No VACUUM afterwards -- that
-- would reclaim the space and defeat the point.

-- ── Phase 4: sessions -- FK to customers; baseline vacuum/analyze, then
--    enough further activity to cross the analyze-staleness threshold, with
--    no re-analyze -> stale-stats ──────────────────────────────────────────

CREATE TABLE complex_test.sessions (
    session_id   bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    customer_id  bigint NOT NULL REFERENCES complex_test.customers(customer_id),
    started_at   timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    page_views   int NOT NULL DEFAULT 1
);

INSERT INTO complex_test.sessions (customer_id, started_at, last_seen_at, page_views)
SELECT 1 + (g % 60000),
       now() - (g || ' seconds')::interval,
       now() - (g || ' seconds')::interval,
       1 + (g % 20)
FROM generate_series(1, 40000) g;

VACUUM ANALYZE complex_test.sessions;
\connect
-- Baseline established. Default autovacuum_analyze_threshold(50) +
-- 0.1 * 40000 live rows = 4050 modifications needed to look "stale". The
-- batch below clears that by more than 6x and is never followed by ANALYZE.

INSERT INTO complex_test.sessions (customer_id, started_at, last_seen_at, page_views)
SELECT 1 + (g % 60000), now(), now(), 1
FROM generate_series(1, 15000) g;

UPDATE complex_test.sessions
SET last_seen_at = now(), page_views = page_views + 1
WHERE session_id % 4 = 0;
\connect
-- ~15000 inserts + ~13750 updates =~ 28750 mods, comfortably over the 4050
-- threshold. No ANALYZE afterwards -> stale-stats candidate. (n_mod_since_analyze
-- is a plain event counter, accurate regardless of the live/dead-tuple quirk
-- above -- no DELETE-vs-UPDATE distinction needed for this signal.)

-- ── Phase 5: reference_codes -- clean control, no FK ─────────────────────────

CREATE TABLE complex_test.reference_codes (
    code        text PRIMARY KEY,
    description text NOT NULL
);

INSERT INTO complex_test.reference_codes (code, description)
SELECT 'CODE-' || g, 'Reference code number ' || g
FROM generate_series(1, 20000) g;

VACUUM ANALYZE complex_test.reference_codes;
\connect
-- Left untouched after this: fresh statistics, no dead tuples. A correct run
-- should report no work for this table in any mode.

-- ── Phase 6: events -- RANGE-partitioned by occurred_at, 4 quarterly
--    partitions, each in a different maintenance state ─────────────────────

CREATE TABLE complex_test.events (
    event_id    bigint GENERATED ALWAYS AS IDENTITY,
    occurred_at timestamptz NOT NULL,
    event_type  text NOT NULL,
    payload     text NOT NULL,
    PRIMARY KEY (event_id, occurred_at)
) PARTITION BY RANGE (occurred_at);
-- RANGE, not LIST: occurred_at is a naturally ordered partition key, and range
-- partitioning is the standard real-world pattern for a time-series-ish event
-- table. It also lets each partition be seeded with one contiguous
-- generate_series() over its own timestamp interval.

CREATE TABLE complex_test.events_2024_q1 PARTITION OF complex_test.events
    FOR VALUES FROM ('2024-01-01') TO ('2024-04-01');
CREATE TABLE complex_test.events_2024_q2 PARTITION OF complex_test.events
    FOR VALUES FROM ('2024-04-01') TO ('2024-07-01');
CREATE TABLE complex_test.events_2024_q3 PARTITION OF complex_test.events
    FOR VALUES FROM ('2024-07-01') TO ('2024-10-01');
CREATE TABLE complex_test.events_2024_q4 PARTITION OF complex_test.events
    FOR VALUES FROM ('2024-10-01') TO ('2025-01-01');
-- No DEFAULT partition: every row generated below has an occurred_at inside
-- one of the four ranges above, so there is nothing for a default partition
-- to catch -- omitting it keeps the partition list exhaustive instead of
-- adding a fifth, permanently-empty partition.
--
-- IMPORTANT: VACUUM/ANALYZE issued against complex_test.events (the parent)
-- automatically recurses into every partition, which would erase the
-- deliberately different states below. Every VACUUM/ANALYZE call in this
-- phase targets a partition BY NAME, never the parent.

-- Q1: bulk-loaded, never vacuumed, never analyzed.
INSERT INTO complex_test.events_2024_q1 (occurred_at, event_type, payload)
SELECT timestamp '2024-01-01' + (g || ' minutes')::interval,
       (ARRAY['click', 'view', 'purchase', 'signup'])[1 + g % 4],
       repeat('e', 120)
FROM generate_series(1, 45000) g;
\connect

-- Q2: loaded once, vacuumed/analyzed, then left alone -- the "clean partition"
-- control, at partition granularity.
INSERT INTO complex_test.events_2024_q2 (occurred_at, event_type, payload)
SELECT timestamp '2024-04-01' + (g || ' minutes')::interval,
       (ARRAY['click', 'view', 'purchase', 'signup'])[1 + g % 4],
       repeat('e', 120)
FROM generate_series(1, 45000) g;
VACUUM ANALYZE complex_test.events_2024_q2;
\connect

-- Q3: vacuumed/analyzed once, then heavily deleted from with no re-vacuum -> bloated.
INSERT INTO complex_test.events_2024_q3 (occurred_at, event_type, payload)
SELECT timestamp '2024-07-01' + (g || ' minutes')::interval,
       (ARRAY['click', 'view', 'purchase', 'signup'])[1 + g % 4],
       repeat('e', 120)
FROM generate_series(1, 45000) g;
VACUUM ANALYZE complex_test.events_2024_q3;
\connect
DELETE FROM complex_test.events_2024_q3
WHERE event_id % 5 != 0;
\connect
ANALYZE complex_test.events_2024_q3;
\connect
-- DELETE, not UPDATE: same reasoning as complex_test.orders above -- see that
-- table's comment for why.

-- Q4: baseline vacuum/analyze, then enough further activity to cross the
-- analyze-staleness threshold, with no re-analyze -> stale-stats.
INSERT INTO complex_test.events_2024_q4 (occurred_at, event_type, payload)
SELECT timestamp '2024-10-01' + (g || ' minutes')::interval,
       (ARRAY['click', 'view', 'purchase', 'signup'])[1 + g % 4],
       repeat('e', 120)
FROM generate_series(1, 20000) g;
VACUUM ANALYZE complex_test.events_2024_q4;
\connect
-- Threshold: 50 + 0.1 * 20000 live rows = 2050 modifications.
INSERT INTO complex_test.events_2024_q4 (occurred_at, event_type, payload)
SELECT timestamp '2024-10-01' + ((20000 + g) || ' minutes')::interval,
       (ARRAY['click', 'view', 'purchase', 'signup'])[1 + g % 4],
       repeat('e', 120)
FROM generate_series(1, 15000) g;
UPDATE complex_test.events_2024_q4
SET event_type = 'view'
WHERE event_id % 3 = 0;
\connect
-- ~15000 inserts + ~11667 updates =~ 26667 mods, more than 10x the 2050
-- threshold. No ANALYZE afterwards.

-- ── Phase 7: daily_rollups -- analyzed but never vacuumed, the inverse of
--    every other "never-X" table in this fixture. Demonstrates that
--    never-vacuumed and never-analyzed are independent signals, not a
--    package deal. Aggregated from the fully-loaded events table, so it must
--    come after Phase 6. ───────────────────────────────────────────────────

CREATE TABLE complex_test_reporting.daily_rollups (
    rollup_day  date NOT NULL,
    event_type  text NOT NULL,
    event_count bigint NOT NULL,
    PRIMARY KEY (rollup_day, event_type)
);

INSERT INTO complex_test_reporting.daily_rollups (rollup_day, event_type, event_count)
SELECT occurred_at::date, event_type, count(*)
FROM complex_test.events
GROUP BY 1, 2;

ANALYZE complex_test_reporting.daily_rollups;
\connect
-- Analyzed but deliberately never VACUUMed:
--   never-vacuumed mode: a candidate (last_vacuum/last_autovacuum are NULL).
--   never-analyzed mode: NOT a candidate (last_analyze is set, from above).

-- ── Summary ──────────────────────────────────────────────────────────────────

\echo ''
\echo '  seeded table state (complex fixture):'
SELECT n.nspname                                              AS schema,
       c.relname                                              AS relation,
       CASE c.relkind
           WHEN 'p' THEN 'partitioned parent'
           WHEN 'r' THEN CASE WHEN c.relispartition THEN 'partition' ELSE 'table' END
           ELSE c.relkind::text
       END                                                    AS kind,
       s.n_live_tup                                           AS live_rows,
       s.n_dead_tup                                            AS dead_rows,
       CASE WHEN s.n_live_tup + s.n_dead_tup = 0 THEN 0
            ELSE round(100.0 * s.n_dead_tup / (s.n_live_tup + s.n_dead_tup), 1)
       END                                                     AS pct_bloat,
       s.n_mod_since_analyze                                   AS mods_since_analyze,
       (s.last_vacuum IS NULL AND s.last_autovacuum IS NULL)   AS never_vacuumed,
       (s.last_analyze IS NULL AND s.last_autoanalyze IS NULL) AS never_analyzed
FROM pg_class c
JOIN pg_namespace n ON n.oid = c.relnamespace
LEFT JOIN pg_stat_user_tables s ON s.relid = c.oid
WHERE n.nspname IN ('complex_test', 'complex_test_reporting')
  AND c.relkind IN ('r', 'p')
ORDER BY n.nspname, c.relname;
