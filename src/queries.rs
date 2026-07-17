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
/// Excludes partitioned parent tables (relkind = 'p').
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = minimum table size in bytes (i64)
///   $3 = maximum table size in bytes (i64)
///   $4 = limit (i64, use i64::MAX for no limit)
pub const FIND_NEVER_VACUUMED: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.last_vacuum     IS NULL
      AND t.last_autovacuum IS NULL
      AND pg_table_size(t.relid) BETWEEN $2 AND $3
    ORDER BY t.n_dead_tup DESC NULLS LAST,
             t.n_live_tup DESC NULLS LAST
    LIMIT $4;
"#;

/// Same as FIND_NEVER_VACUUMED but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = minimum table size in bytes (i64)
///   $4 = maximum table size in bytes (i64)
pub const FIND_NEVER_VACUUMED_TABLE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND t.relname = $2
      AND c.relkind != 'p'
      AND t.last_vacuum     IS NULL
      AND t.last_autovacuum IS NULL
      AND pg_table_size(t.relid) BETWEEN $3 AND $4
    ORDER BY t.n_dead_tup DESC NULLS LAST,
             t.n_live_tup DESC NULLS LAST;
"#;

/// Tables that have NEVER been analyzed (neither manual nor autoanalyze).
///
/// Ordered by estimated live row count descending (largest tables first).
/// Excludes partitioned parent tables (relkind = 'p').
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = minimum table size in bytes (i64)
///   $3 = maximum table size in bytes (i64)
///   $4 = limit (i64, use i64::MAX for no limit)
pub const FIND_NEVER_ANALYZED: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1) AS n_live_tup,
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.last_analyze     IS NULL
      AND t.last_autoanalyze IS NULL
      AND pg_table_size(t.relid) BETWEEN $2 AND $3
    ORDER BY t.n_live_tup DESC NULLS LAST
    LIMIT $4;
"#;

/// Same as FIND_NEVER_ANALYZED but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = minimum table size in bytes (i64)
///   $4 = maximum table size in bytes (i64)
pub const FIND_NEVER_ANALYZED_TABLE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1) AS n_live_tup,
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND t.relname = $2
      AND c.relkind != 'p'
      AND t.last_analyze     IS NULL
      AND t.last_autoanalyze IS NULL
      AND pg_table_size(t.relid) BETWEEN $3 AND $4
    ORDER BY t.n_live_tup DESC NULLS LAST;
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
///   $5 = limit (i64, use i64::MAX for no limit)
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
    ORDER BY age(c.relfrozenxid) DESC
    LIMIT $5;
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
pub const GET_FREEZE_MAX_AGE: &str = "SELECT current_setting('autovacuum_freeze_max_age')::bigint";

/// The server's numeric version (server_version_num), e.g. 160003 for 16.3.
/// Used by connection::connect() to enforce the PostgreSQL 14 minimum.
pub const GET_SERVER_VERSION_NUM: &str = "SELECT current_setting('server_version_num')::int";

/// The server's autovacuum_analyze_threshold and autovacuum_analyze_scale_factor
/// settings. Used as the default stale-stats thresholds unless overridden by
/// --analyze-threshold / --analyze-scale-factor.
pub const GET_ANALYZE_SETTINGS: &str = r#"
    SELECT
        current_setting('autovacuum_analyze_threshold')::bigint    AS analyze_threshold,
        current_setting('autovacuum_analyze_scale_factor')::float8 AS analyze_scale_factor
"#;

/// Tables with excessive dead tuples (bloat candidates).
///
/// Ordered by bloat percentage descending (worst first).
/// Excludes partitioned parent tables (relkind = 'p').
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = bloat threshold percentage (float8)
///   $3 = minimum dead tuple count (i64)
///   $4 = minimum table size in bytes (i64)
///   $5 = maximum table size in bytes (i64)
///   $6 = limit (i64, use i64::MAX for no limit)
pub const FIND_BLOAT_CANDIDATES: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.n_dead_tup >= $3
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
      AND (100.0 * t.n_dead_tup / NULLIF(t.n_live_tup + t.n_dead_tup, 0)) >= $2::float8
    ORDER BY (100.0 * t.n_dead_tup / NULLIF(t.n_live_tup + t.n_dead_tup, 0)) DESC
    LIMIT $6;
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
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND t.relname = $2
      AND c.relkind != 'p'
      AND t.n_dead_tup >= $4
      AND pg_table_size(t.relid) BETWEEN $5 AND $6
      AND (100.0 * t.n_dead_tup / NULLIF(t.n_live_tup + t.n_dead_tup, 0)) >= $3::float8
    ORDER BY (100.0 * t.n_dead_tup / NULLIF(t.n_live_tup + t.n_dead_tup, 0)) DESC;
"#;

/// PIDs of active VACUUM or autovacuum workers currently operating on a specific table.
///
/// Joins pg_stat_progress_vacuum to pg_stat_activity, pg_class, and pg_namespace
/// to identify the exact table. Returns both the PID and backend_type so the caller
/// can distinguish autovacuum workers from manual VACUUM sessions.
/// Parameters:
///   $1 = schema name (text)
///   $2 = table name (text)
pub const FIND_ACTIVE_VACUUMS_ON_TABLE: &str = r#"
    SELECT psa.pid, psa.backend_type
    FROM pg_stat_progress_vacuum ppv
    JOIN pg_stat_activity psa ON psa.pid  = ppv.pid
    JOIN pg_class         pc  ON pc.oid   = ppv.relid
    JOIN pg_namespace     pn  ON pn.oid   = pc.relnamespace
    WHERE pn.nspname = $1
      AND pc.relname = $2
"#;

/// Tables where enough rows have changed since the last ANALYZE that planner
/// statistics are likely stale, based on the same math PostgreSQL's own
/// autovacuum uses (analyze_threshold + analyze_scale_factor * n_live_tup).
///
/// Ordered by n_mod_since_analyze descending (most drift first).
/// Excludes partitioned parent tables (relkind = 'p').
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = flat modification-count floor (bigint)
///   $3 = scale factor applied to live row count (float8)
///   $4 = minimum table size in bytes (i64)
///   $5 = maximum table size in bytes (i64)
///   $6 = limit (i64, use i64::MAX for no limit)
pub const FIND_STALE_STATS: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)          AS n_live_tup,
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
      AND t.n_mod_since_analyze > ($2::bigint + $3::float8 * COALESCE(t.n_live_tup, 0))
    ORDER BY t.n_mod_since_analyze DESC
    LIMIT $6;
"#;

/// Same as FIND_STALE_STATS but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = flat modification-count floor (bigint)
///   $4 = scale factor applied to live row count (float8)
///   $5 = minimum table size in bytes (i64)
///   $6 = maximum table size in bytes (i64)
pub const FIND_STALE_STATS_TABLE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)          AS n_live_tup,
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND t.relname = $2
      AND c.relkind != 'p'
      AND pg_table_size(t.relid) BETWEEN $5 AND $6
      AND t.n_mod_since_analyze > ($3::bigint + $4::float8 * COALESCE(t.n_live_tup, 0))
    ORDER BY t.n_mod_since_analyze DESC;
"#;

/// Get the dead tuple count for a specific table.
/// Parameters:
///   $1 = schema name (text)
///   $2 = table name (text)
pub const GET_DEAD_TUPLE_COUNT: &str = r#"
    SELECT COALESCE(n_dead_tup, 0) AS n_dead_tup
    FROM pg_stat_user_tables
    WHERE schemaname = $1 AND relname = $2;
"#;

/// Insert a maintenance operation log entry into maintainer_logbook.
/// Parameters:
///   $1 = schema_name (text)
///   $2 = table_name (text)
///   $3 = operation (text) — "VACUUM", "ANALYZE", or "FREEZE"
///   $4 = mode (text) — "never-vacuumed", "bloated", "wraparound", "never-analyzed", or "stale-stats"
///   $5 = status (text) — "success" or "error"
///   $6 = dead_tuples_before (bigint, nullable)
///   $7 = dead_tuples_removed (bigint, nullable)
///   $8 = duration_ms (bigint)
///   $9 = error_message (text, nullable)
pub const INSERT_MAINTENANCE_LOG: &str = r#"
    INSERT INTO maintainer_logbook.maintenance_logbook
      (run_started_at, schema_name, table_name, operation, mode, status,
       dead_tuples_before, dead_tuples_removed, duration_ms, error_message)
    VALUES
      (now(), $1, $2, $3, $4, $5, $6, $7, $8, $9)
"#;
