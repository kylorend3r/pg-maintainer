-- Fixtures for exercising needs-vacuum (classic + max-threshold pass) and prevent-wraparound.
-- Manual use only (never loaded by CI). Everything lives in schema nv_fixtures:
--   DROP SCHEMA nv_fixtures CASCADE;   -- cleans it all up
--
-- Run:  psql -h localhost -U dba -d vacuum_platform -f testing/schema/needs_vacuum_fixtures.sql
-- Then: cargo run -- -d vacuum_platform -s nv_fixtures --mode needs-vacuum --dry-run

DROP SCHEMA IF EXISTS nv_fixtures CASCADE;
CREATE SCHEMA nv_fixtures;

-- 1. Classic hit: 100k dead tuples > 50 + 0.2 * live. Found by BOTH passes.
CREATE TABLE nv_fixtures.dead_classic (id int) WITH (autovacuum_enabled = false);
INSERT INTO nv_fixtures.dead_classic SELECT generate_series(1, 200000);
ANALYZE nv_fixtures.dead_classic;
DELETE FROM nv_fixtures.dead_classic WHERE id <= 100000;

-- 2. Per-table reloptions: its own threshold (1M dead tuples) is far from crossed and
--    its insert trigger is disabled, so the max-threshold pass must NOT flag it. The
--    classic pass still does (it only knows the global 50 + 0.2 * live), which is
--    the one case where the merged list can contain a table autovacuum would skip.
CREATE TABLE nv_fixtures.reloption_high_thr (id int)
    WITH (autovacuum_enabled = false,
          autovacuum_vacuum_threshold = 1000000,
          autovacuum_vacuum_insert_threshold = -1);
INSERT INTO nv_fixtures.reloption_high_thr SELECT generate_series(1, 20000);
ANALYZE nv_fixtures.reloption_high_thr;
DELETE FROM nv_fixtures.reloption_high_thr WHERE id <= 10000;

-- 3. Insert-trigger only: append-only, zero dead tuples. Classic pass misses it;
--    the max-threshold pass adds it (n_ins_since_vacuum > 1000 + 0.2 * reltuples).
CREATE TABLE nv_fixtures.insert_only (id int) WITH (autovacuum_enabled = false);
INSERT INTO nv_fixtures.insert_only SELECT generate_series(1, 50000);

-- 4. Healthy control: must appear in neither pass.
CREATE TABLE nv_fixtures.healthy (id int) WITH (autovacuum_enabled = false);
INSERT INTO nv_fixtures.healthy SELECT generate_series(1, 10);
VACUUM nv_fixtures.healthy;

-- Stats are flushed asynchronously; give them a moment.
SELECT pg_sleep(2);
SELECT relname, n_live_tup, n_dead_tup, n_ins_since_vacuum
FROM pg_stat_user_tables WHERE schemaname = 'nv_fixtures' ORDER BY relname;

-- ---------------------------------------------------------------------------
-- XID aging for prevent-wraparound (optional; run separately).
--
-- burn_xids(n) assigns n transaction IDs using subtransactions (an EXCEPTION
-- block opens one, and a transactional pg_logical_emit_message() forces an XID), so it is far faster
-- than n separate transactions. COMMIT every 100k keeps each transaction small.
--
-- SAFETY: this ages the WHOLE cluster, not just the fixture table.
--  * Autovacuum still launches anti-wraparound vacuums once age(relfrozenxid) >
--    autovacuum_freeze_max_age (default 200M) even for autovacuum_enabled=false
--    tables. To keep the table "wraparound-eligible" without that happening, burn
--    LESS than 200M and run the tool with a lower --wraparound-min-age, e.g.
--    --wraparound-min-age 5000000 after burning 6M.
--  * Never push anywhere near 2^31 (2.1 billion) on a database you care about.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE PROCEDURE nv_fixtures.burn_xids(n bigint)
LANGUAGE plpgsql AS $$
DECLARE
    i bigint := 0;
BEGIN
    WHILE i < n LOOP
        BEGIN
            PERFORM pg_logical_emit_message(true, 'burn', '');  -- transactional message: assigns the subxact its own XID
        EXCEPTION WHEN OTHERS THEN NULL;     -- the EXCEPTION clause creates the subxact
        END;
        i := i + 1;
        IF i % 100000 = 0 THEN COMMIT; END IF;
    END LOOP;
END $$;

CREATE TABLE nv_fixtures.wraparound_victim (id int) WITH (autovacuum_enabled = false);
INSERT INTO nv_fixtures.wraparound_victim SELECT generate_series(1, 1000);

-- Uncomment to age the table by ~6M XIDs, then check:
-- CALL nv_fixtures.burn_xids(6000000);
-- SELECT relname, age(relfrozenxid) FROM pg_class WHERE relname = 'wraparound_victim';
-- cargo run -- -d vacuum_platform -s nv_fixtures --mode prevent-wraparound --wraparound-min-age 5000000 --dry-run
