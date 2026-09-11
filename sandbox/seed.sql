-- Demo data for the pg-maintainer sandbox.
--
-- Each table is left in a state that trips exactly one discovery mode, so a run
-- against this database produces a readable, reproducible result. This relies on
-- autovacuum being off (see sandbox/entrypoint.sh) — otherwise the background
-- worker would clean these up and stamp every table as recently vacuumed.
--
-- The `\connect` calls between phases are load-bearing. Cumulative counters such
-- as n_tup_ins are flushed to the shared statistics store asynchronously, while
-- VACUUM and ANALYZE *set* n_live_tup outright. Without a flush in between, the
-- bulk-load deltas land on top of the value VACUUM just set and the counts come
-- out roughly doubled. Ending the session forces the pending deltas out first.

-- ── Phase 1: load ────────────────────────────────────────────────────────────

\echo '  loading tables ...'

DROP TABLE IF EXISTS fresh_signups, bloated_events, stale_customers, quiet_archive;

CREATE TABLE fresh_signups AS
SELECT g                                   AS id,
       'user' || g || '@example.com'       AS email,
       now() - (g || ' minutes')::interval AS signed_up_at
FROM generate_series(1, 50000) g;
-- Deliberately never vacuumed or analyzed: this is what modes 1 and 2 look for.

CREATE TABLE bloated_events AS
SELECT g                      AS id,
       (g % 7)                AS kind,
       repeat('payload-', 20) AS payload
FROM generate_series(1, 200000) g;

CREATE TABLE stale_customers AS
SELECT g                  AS id,
       'customer ' || g   AS name,
       (g % 100)::numeric AS balance
FROM generate_series(1, 5000) g;

CREATE TABLE quiet_archive AS
SELECT g AS id, md5(g::text) AS checksum
FROM generate_series(1, 10000) g;

\connect demo

-- ── Phase 2: baseline, so these three are not "never vacuumed" ───────────────

\echo '  establishing a clean baseline ...'

VACUUM ANALYZE bloated_events;
VACUUM ANALYZE stale_customers;
VACUUM ANALYZE quiet_archive;

\connect demo

-- ── Phase 3: dirty them, each in exactly one way ─────────────────────────────

\echo '  creating bloat and stale statistics ...'

-- ~90% of rows become dead tuples. With autovacuum off they stay that way.
DELETE FROM bloated_events WHERE id % 10 <> 0;

-- Far more modifications than analyze_threshold + 0.1 * live_rows, so the
-- planner's statistics for this table are now stale.
INSERT INTO stale_customers
SELECT g, 'customer ' || g, (g % 100)::numeric
FROM generate_series(5001, 25000) g;
UPDATE stale_customers SET balance = balance + 1 WHERE id % 3 = 0;

-- quiet_archive is left untouched on purpose: no dead tuples, fresh statistics.
-- A correct run should report no work for it in any mode.

\connect demo

-- ── Summary ──────────────────────────────────────────────────────────────────

\echo ''
\echo '  seeded table state:'
SELECT relname             AS table_name,
       n_live_tup          AS live_rows,
       n_dead_tup          AS dead_rows,
       CASE WHEN n_live_tup + n_dead_tup = 0 THEN 0
            ELSE round(100.0 * n_dead_tup / (n_live_tup + n_dead_tup), 1)
       END                 AS pct_bloat,
       n_mod_since_analyze AS mods_since_analyze,
       (last_vacuum IS NULL AND last_autovacuum IS NULL)   AS never_vacuumed,
       (last_analyze IS NULL AND last_autoanalyze IS NULL) AS never_analyzed
FROM pg_stat_user_tables
ORDER BY relname;
