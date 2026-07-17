-- Test fixture schema for pg-maintainer
-- Idempotent setup script that creates tables for testing all four maintenance modes

DROP SCHEMA IF EXISTS pgm_test CASCADE;
CREATE SCHEMA pgm_test;

-- Phase 1 + Phase 2 candidate: bulk-loaded, never vacuumed or analyzed.
CREATE TABLE pgm_test.never_maintained (id bigint, payload text);
INSERT INTO pgm_test.never_maintained
  SELECT g, repeat('x', 100) FROM generate_series(1, 200000) g;
-- deliberately: no VACUUM, no ANALYZE run against this table.

-- Phase 4 candidate: vacuumed/analyzed once, then churned to build dead tuples.
CREATE TABLE pgm_test.bloated (id bigint, payload text);
INSERT INTO pgm_test.bloated
  SELECT g, repeat('y', 100) FROM generate_series(1, 200000) g;
VACUUM ANALYZE pgm_test.bloated;
UPDATE pgm_test.bloated SET payload = repeat('z', 100) WHERE id % 5 != 0;
-- ~80% churn -> n_dead_tup / (n_live_tup+n_dead_tup) ~ 0.8
-- no vacuum afterwards, so it accumulates dead tuples

-- Size-filter edge cases: a tiny table and a padded larger one.
CREATE TABLE pgm_test.tiny (id int);
INSERT INTO pgm_test.tiny SELECT 1;
VACUUM ANALYZE pgm_test.tiny;

CREATE TABLE pgm_test.padded (id bigint, payload text);
INSERT INTO pgm_test.padded
  SELECT g, repeat('p', 1000) FROM generate_series(1, 500000) g;
VACUUM ANALYZE pgm_test.padded;
-- ~500MB+, use with --max-table-size-gb to exclude it
