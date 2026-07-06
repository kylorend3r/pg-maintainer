// Session setup
pub const SET_STATEMENT_TIMEOUT: &str = "SET statement_timeout TO 0";
pub const SET_IDLE_SESSION_TIMEOUT: &str = "SET idle_session_timeout TO 0";
pub const SET_APPLICATION_NAME: &str = "SET application_name TO 'pg-maintainer'";

// Schema discovery — excludes system and tool-managed schemas
pub const GET_ALL_USER_SCHEMAS: &str = r#"
    SELECT nspname
    FROM pg_namespace
    WHERE nspname NOT IN (
        'pg_catalog', 'information_schema', 'pg_toast',
        'pg_temp_1', 'pg_toast_temp_1'
    )
    AND nspname NOT LIKE 'pg_temp_%'
    AND nspname NOT LIKE 'pg_toast_temp_%'
    ORDER BY nspname;
"#;

/// Tables that have NEVER been vacuumed (neither manual nor autovacuum).
///
/// Ordered by dead tuple count descending so the most bloated tables come first.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = minimum table size in bytes (i64)
///   $3 = maximum table size in bytes (i64)
pub const FIND_NEVER_VACUUMED: &str = r#"
    SELECT
        schemaname,
        relname AS tablename,
        COALESCE(n_live_tup, -1)  AS n_live_tup,
        COALESCE(n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables
    WHERE schemaname = ANY($1::text[])
      AND last_vacuum     IS NULL
      AND last_autovacuum IS NULL
      AND pg_table_size(relid) BETWEEN $2 AND $3
    ORDER BY n_dead_tup DESC NULLS LAST,
             n_live_tup DESC NULLS LAST;
"#;

/// Same as FIND_NEVER_VACUUMED but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = minimum table size in bytes (i64)
///   $4 = maximum table size in bytes (i64)
pub const FIND_NEVER_VACUUMED_TABLE: &str = r#"
    SELECT
        schemaname,
        relname AS tablename,
        COALESCE(n_live_tup, -1)  AS n_live_tup,
        COALESCE(n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables
    WHERE schemaname = ANY($1::text[])
      AND relname = $2
      AND last_vacuum     IS NULL
      AND last_autovacuum IS NULL
      AND pg_table_size(relid) BETWEEN $3 AND $4
    ORDER BY n_dead_tup DESC NULLS LAST,
             n_live_tup DESC NULLS LAST;
"#;

/// Tables that have NEVER been analyzed (neither manual nor autoanalyze).
///
/// Ordered by estimated live row count descending (largest tables first).
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = minimum table size in bytes (i64)
///   $3 = maximum table size in bytes (i64)
pub const FIND_NEVER_ANALYZED: &str = r#"
    SELECT
        schemaname,
        relname AS tablename,
        COALESCE(n_live_tup, -1) AS n_live_tup,
        COALESCE(n_dead_tup, -1) AS n_dead_tup
    FROM pg_stat_user_tables
    WHERE schemaname = ANY($1::text[])
      AND last_analyze     IS NULL
      AND last_autoanalyze IS NULL
      AND pg_table_size(relid) BETWEEN $2 AND $3
    ORDER BY n_live_tup DESC NULLS LAST;
"#;

/// Same as FIND_NEVER_ANALYZED but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = minimum table size in bytes (i64)
///   $4 = maximum table size in bytes (i64)
pub const FIND_NEVER_ANALYZED_TABLE: &str = r#"
    SELECT
        schemaname,
        relname AS tablename,
        COALESCE(n_live_tup, -1) AS n_live_tup,
        COALESCE(n_dead_tup, -1) AS n_dead_tup
    FROM pg_stat_user_tables
    WHERE schemaname = ANY($1::text[])
      AND relname = $2
      AND last_analyze     IS NULL
      AND last_autoanalyze IS NULL
      AND pg_table_size(relid) BETWEEN $3 AND $4
    ORDER BY n_live_tup DESC NULLS LAST;
"#;

/// Tables whose transaction age has exceeded autovacuum_freeze_max_age and therefore
/// need an aggressive VACUUM FREEZE to push back the wraparound horizon.
///
/// Includes regular tables ('r'), TOAST tables ('t'), and materialized views ('m').
/// System schemas (pg_catalog, information_schema, pg_toast) are excluded because
/// PostgreSQL manages freezing for those itself.
///
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = minimum XID age threshold (bigint) — defaults to autovacuum_freeze_max_age
///   $3 = minimum table size in bytes (i64)
///   $4 = maximum table size in bytes (i64)
pub const FIND_WRAPAROUND_CANDIDATES: &str = r#"
    SELECT
        n.nspname                                               AS schema_name,
        c.relname                                               AS table_name,
        age(c.relfrozenxid)::bigint                             AS xid_age,
        current_setting('autovacuum_freeze_max_age')::bigint    AS freeze_max_age
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relkind IN ('r', 't', 'm')
      AND n.nspname = ANY($1::text[])
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND age(c.relfrozenxid) > $2::bigint
      AND pg_table_size(c.oid) BETWEEN $3 AND $4
    ORDER BY age(c.relfrozenxid) DESC;
"#;

/// Same as FIND_WRAPAROUND_CANDIDATES but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = minimum XID age threshold (bigint)
///   $3 = table name (text)
///   $4 = minimum table size in bytes (i64)
///   $5 = maximum table size in bytes (i64)
pub const FIND_WRAPAROUND_CANDIDATES_TABLE: &str = r#"
    SELECT
        n.nspname                                               AS schema_name,
        c.relname                                               AS table_name,
        age(c.relfrozenxid)::bigint                             AS xid_age,
        current_setting('autovacuum_freeze_max_age')::bigint    AS freeze_max_age
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.relkind IN ('r', 't', 'm')
      AND n.nspname = ANY($1::text[])
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND age(c.relfrozenxid) > $2::bigint
      AND c.relname = $3
      AND pg_table_size(c.oid) BETWEEN $4 AND $5
    ORDER BY age(c.relfrozenxid) DESC;
"#;

/// The server's autovacuum_freeze_max_age setting in transactions.
/// Used to convert a wraparound-percentage threshold into an absolute XID age.
pub const GET_FREEZE_MAX_AGE: &str =
    "SELECT current_setting('autovacuum_freeze_max_age')::bigint";

/// Tables with excessive dead tuples (bloat candidates).
///
/// Ordered by bloat percentage descending (worst first).
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = bloat threshold percentage (float8)
///   $3 = minimum dead tuple count (i64)
///   $4 = minimum table size in bytes (i64)
///   $5 = maximum table size in bytes (i64)
pub const FIND_BLOAT_CANDIDATES: &str = r#"
    SELECT
        schemaname,
        relname AS tablename,
        COALESCE(n_live_tup, -1)  AS n_live_tup,
        COALESCE(n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables
    WHERE schemaname = ANY($1::text[])
      AND n_dead_tup >= $3
      AND pg_table_size(relid) BETWEEN $4 AND $5
      AND (100.0 * n_dead_tup / NULLIF(n_live_tup + n_dead_tup, 0)) >= $2::float8
    ORDER BY (100.0 * n_dead_tup / NULLIF(n_live_tup + n_dead_tup, 0)) DESC;
"#;

/// Same as FIND_BLOAT_CANDIDATES but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = bloat threshold percentage (float8)
///   $4 = minimum dead tuple count (i64)
///   $5 = minimum table size in bytes (i64)
///   $6 = maximum table size in bytes (i64)
pub const FIND_BLOAT_CANDIDATES_TABLE: &str = r#"
    SELECT
        schemaname,
        relname AS tablename,
        COALESCE(n_live_tup, -1)  AS n_live_tup,
        COALESCE(n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables
    WHERE schemaname = ANY($1::text[])
      AND relname = $2
      AND n_dead_tup >= $4
      AND pg_table_size(relid) BETWEEN $5 AND $6
      AND (100.0 * n_dead_tup / NULLIF(n_live_tup + n_dead_tup, 0)) >= $3::float8
    ORDER BY (100.0 * n_dead_tup / NULLIF(n_live_tup + n_dead_tup, 0)) DESC;
"#;

/// PIDs of active VACUUM or autovacuum workers currently operating on a specific table.
///
/// Joins pg_stat_progress_vacuum to pg_stat_activity, pg_class, and pg_namespace
/// to identify the exact table. Both manual VACUUM and autovacuum workers appear here.
/// Parameters:
///   $1 = schema name (text)
///   $2 = table name (text)
pub const FIND_ACTIVE_VACUUMS_ON_TABLE: &str = r#"
    SELECT psa.pid
    FROM pg_stat_progress_vacuum ppv
    JOIN pg_stat_activity psa ON psa.pid  = ppv.pid
    JOIN pg_class         pc  ON pc.oid   = ppv.relid
    JOIN pg_namespace     pn  ON pn.oid   = pc.relnamespace
    WHERE pn.nspname = $1
      AND pc.relname = $2
"#;
