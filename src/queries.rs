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
        'pg_temp_1', 'pg_toast_temp_1', 'maintainer_logbook'
    )
    AND nspname NOT LIKE 'pg_temp_%'
    AND nspname NOT LIKE 'pg_toast_temp_%'
    ORDER BY nspname;
"#;

/// Tables that have NEVER been vacuumed (neither manual nor autovacuum).
///
/// Default ordering: dead tuple count descending so the most bloated tables come first.
/// Excludes partitioned parent tables (relkind = 'p'). `last_maintained` is always
/// NULL for this mode by definition (candidacy requires both timestamps NULL) — it
/// is still returned so the shared row-mapping code in operations.rs works unchanged.
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
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
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

/// Same as FIND_NEVER_VACUUMED, ordered by table size descending (largest first).
/// Parameters: same as FIND_NEVER_VACUUMED.
pub const FIND_NEVER_VACUUMED_BY_SIZE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.last_vacuum     IS NULL
      AND t.last_autovacuum IS NULL
      AND pg_table_size(t.relid) BETWEEN $2 AND $3
    ORDER BY size_bytes DESC
    LIMIT $4;
"#;

/// Same as FIND_NEVER_VACUUMED, ordered by last-maintained ascending (NULLS FIRST).
/// Every row is NULL for this mode by definition, so this ordering is a no-op tie —
/// still provided for API symmetry across all 5 modes.
/// Parameters: same as FIND_NEVER_VACUUMED.
pub const FIND_NEVER_VACUUMED_BY_LAST_MAINTAINED: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.last_vacuum     IS NULL
      AND t.last_autovacuum IS NULL
      AND pg_table_size(t.relid) BETWEEN $2 AND $3
    ORDER BY last_maintained ASC NULLS FIRST
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
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
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
/// Default ordering: estimated live row count descending (largest tables first).
/// Excludes partitioned parent tables (relkind = 'p'). `last_maintained` is always
/// NULL for this mode by definition (candidacy requires both timestamps NULL) — it
/// is still returned so the shared row-mapping code in operations.rs works unchanged.
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
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup,
        pg_table_size(t.relid)     AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
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

/// Same as FIND_NEVER_ANALYZED, ordered by table size descending (largest first).
/// Parameters: same as FIND_NEVER_ANALYZED.
pub const FIND_NEVER_ANALYZED_BY_SIZE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1) AS n_live_tup,
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup,
        pg_table_size(t.relid)     AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.last_analyze     IS NULL
      AND t.last_autoanalyze IS NULL
      AND pg_table_size(t.relid) BETWEEN $2 AND $3
    ORDER BY size_bytes DESC
    LIMIT $4;
"#;

/// Same as FIND_NEVER_ANALYZED, ordered by last-maintained ascending (NULLS FIRST).
/// Every row is NULL for this mode by definition, so this ordering is a no-op tie —
/// still provided for API symmetry across all 5 modes.
/// Parameters: same as FIND_NEVER_ANALYZED.
pub const FIND_NEVER_ANALYZED_BY_LAST_MAINTAINED: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1) AS n_live_tup,
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup,
        pg_table_size(t.relid)     AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.last_analyze     IS NULL
      AND t.last_autoanalyze IS NULL
      AND pg_table_size(t.relid) BETWEEN $2 AND $3
    ORDER BY last_maintained ASC NULLS FIRST
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
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup,
        pg_table_size(t.relid)     AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
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
/// Default ordering: XID age descending (worst first). Uses pg_stat_all_tables (not
/// pg_stat_user_tables) for last_maintained because the candidate set includes TOAST
/// tables, which live outside pg_stat_user_tables's schema filter.
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
        current_setting('autovacuum_freeze_max_age')::bigint    AS freeze_max_age,
        pg_table_size(c.oid)                                    AS size_bytes,
        GREATEST(su.last_vacuum, su.last_autovacuum)            AS last_maintained
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_stat_all_tables su ON su.relid = c.oid
    WHERE c.relkind IN ('r', 't', 'm')
      AND n.nspname = ANY($1::text[])
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND age(c.relfrozenxid) > $2::bigint
      AND pg_table_size(c.oid) BETWEEN $3 AND $4
    ORDER BY age(c.relfrozenxid) DESC
    LIMIT $5;
"#;

/// Same as FIND_WRAPAROUND_CANDIDATES, ordered by table size descending (largest first).
/// Parameters: same as FIND_WRAPAROUND_CANDIDATES.
pub const FIND_WRAPAROUND_CANDIDATES_BY_SIZE: &str = r#"
    SELECT
        n.nspname                                               AS schema_name,
        c.relname                                               AS table_name,
        age(c.relfrozenxid)::bigint                             AS xid_age,
        current_setting('autovacuum_freeze_max_age')::bigint    AS freeze_max_age,
        pg_table_size(c.oid)                                    AS size_bytes,
        GREATEST(su.last_vacuum, su.last_autovacuum)            AS last_maintained
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_stat_all_tables su ON su.relid = c.oid
    WHERE c.relkind IN ('r', 't', 'm')
      AND n.nspname = ANY($1::text[])
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND age(c.relfrozenxid) > $2::bigint
      AND pg_table_size(c.oid) BETWEEN $3 AND $4
    ORDER BY size_bytes DESC
    LIMIT $5;
"#;

/// Same as FIND_WRAPAROUND_CANDIDATES, ordered by last-maintained ascending (oldest/never first).
/// Parameters: same as FIND_WRAPAROUND_CANDIDATES.
pub const FIND_WRAPAROUND_CANDIDATES_BY_LAST_MAINTAINED: &str = r#"
    SELECT
        n.nspname                                               AS schema_name,
        c.relname                                               AS table_name,
        age(c.relfrozenxid)::bigint                             AS xid_age,
        current_setting('autovacuum_freeze_max_age')::bigint    AS freeze_max_age,
        pg_table_size(c.oid)                                    AS size_bytes,
        GREATEST(su.last_vacuum, su.last_autovacuum)            AS last_maintained
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_stat_all_tables su ON su.relid = c.oid
    WHERE c.relkind IN ('r', 't', 'm')
      AND n.nspname = ANY($1::text[])
      AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
      AND age(c.relfrozenxid) > $2::bigint
      AND pg_table_size(c.oid) BETWEEN $3 AND $4
    ORDER BY last_maintained ASC NULLS FIRST
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
        current_setting('autovacuum_freeze_max_age')::bigint    AS freeze_max_age,
        pg_table_size(c.oid)                                    AS size_bytes,
        GREATEST(su.last_vacuum, su.last_autovacuum)            AS last_maintained
    FROM pg_class c
    JOIN pg_namespace n ON n.oid = c.relnamespace
    LEFT JOIN pg_stat_all_tables su ON su.relid = c.oid
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
/// Default ordering: bloat percentage descending (worst first).
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
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
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

/// Same as FIND_BLOAT_CANDIDATES, ordered by table size descending (largest first).
/// Parameters: same as FIND_BLOAT_CANDIDATES.
pub const FIND_BLOAT_CANDIDATES_BY_SIZE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.n_dead_tup >= $3
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
      AND (100.0 * t.n_dead_tup / NULLIF(t.n_live_tup + t.n_dead_tup, 0)) >= $2::float8
    ORDER BY size_bytes DESC
    LIMIT $6;
"#;

/// Same as FIND_BLOAT_CANDIDATES, ordered by last-maintained ascending (oldest/never first).
/// Parameters: same as FIND_BLOAT_CANDIDATES.
pub const FIND_BLOAT_CANDIDATES_BY_LAST_MAINTAINED: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)  AS n_live_tup,
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND t.n_dead_tup >= $3
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
      AND (100.0 * t.n_dead_tup / NULLIF(t.n_live_tup + t.n_dead_tup, 0)) >= $2::float8
    ORDER BY last_maintained ASC NULLS FIRST
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
        COALESCE(t.n_dead_tup, -1)  AS n_dead_tup,
        pg_table_size(t.relid)      AS size_bytes,
        GREATEST(t.last_vacuum, t.last_autovacuum) AS last_maintained
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
/// Default ordering: n_mod_since_analyze descending (most drift first).
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
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze,
        pg_table_size(t.relid)              AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
      AND t.n_mod_since_analyze > ($2::bigint + $3::float8 * COALESCE(t.n_live_tup, 0))
    ORDER BY t.n_mod_since_analyze DESC
    LIMIT $6;
"#;

/// Same as FIND_STALE_STATS, ordered by table size descending (largest first).
/// Parameters: same as FIND_STALE_STATS.
pub const FIND_STALE_STATS_BY_SIZE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)          AS n_live_tup,
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze,
        pg_table_size(t.relid)              AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
      AND t.n_mod_since_analyze > ($2::bigint + $3::float8 * COALESCE(t.n_live_tup, 0))
    ORDER BY size_bytes DESC
    LIMIT $6;
"#;

/// Same as FIND_STALE_STATS, ordered by last-maintained ascending (oldest/never first).
/// Parameters: same as FIND_STALE_STATS.
pub const FIND_STALE_STATS_BY_LAST_MAINTAINED: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)          AS n_live_tup,
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze,
        pg_table_size(t.relid)              AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
      AND t.n_mod_since_analyze > ($2::bigint + $3::float8 * COALESCE(t.n_live_tup, 0))
    ORDER BY last_maintained ASC NULLS FIRST
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
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze,
        pg_table_size(t.relid)              AS size_bytes,
        GREATEST(t.last_analyze, t.last_autoanalyze) AS last_maintained
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND t.relname = $2
      AND c.relkind != 'p'
      AND pg_table_size(t.relid) BETWEEN $5 AND $6
      AND t.n_mod_since_analyze > ($3::bigint + $4::float8 * COALESCE(t.n_live_tup, 0))
    ORDER BY t.n_mod_since_analyze DESC;
"#;

/// Tables whose most recent VACUUM (manual or auto) is older than the configured
/// number of days. Never-vacuumed tables are excluded (GREATEST returns NULL when
/// both inputs are NULL).
///
/// Ordered by age ascending (oldest first).
/// Excludes partitioned parent tables (relkind = 'p').
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = number of days (int)
///   $3 = minimum table size in bytes (i64)
///   $4 = maximum table size in bytes (i64)
///   $5 = limit (i64, use i64::MAX for no limit)
pub const FIND_VACUUM_OVERDUE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1) AS n_live_tup,
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup,
        EXTRACT(EPOCH FROM now() - GREATEST(t.last_vacuum, t.last_autovacuum))::float8
            / 86400.0 AS days_since_vacuum
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND GREATEST(t.last_vacuum, t.last_autovacuum) < now() - make_interval(days => $2::int)
      AND pg_table_size(t.relid) BETWEEN $3 AND $4
    ORDER BY GREATEST(t.last_vacuum, t.last_autovacuum) ASC
    LIMIT $5;
"#;

/// Same as FIND_VACUUM_OVERDUE but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = number of days (int)
///   $4 = minimum table size in bytes (i64)
///   $5 = maximum table size in bytes (i64)
pub const FIND_VACUUM_OVERDUE_TABLE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1) AS n_live_tup,
        COALESCE(t.n_dead_tup, -1) AS n_dead_tup,
        EXTRACT(EPOCH FROM now() - GREATEST(t.last_vacuum, t.last_autovacuum))::float8
            / 86400.0 AS days_since_vacuum
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND t.relname = $2
      AND c.relkind != 'p'
      AND GREATEST(t.last_vacuum, t.last_autovacuum) < now() - make_interval(days => $3::int)
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
    ORDER BY GREATEST(t.last_vacuum, t.last_autovacuum) ASC;
"#;

/// Tables whose most recent ANALYZE (manual or auto) is older than the configured
/// number of days. Never-analyzed tables are excluded (GREATEST returns NULL when
/// both inputs are NULL).
///
/// Ordered by age ascending (oldest first).
/// Excludes partitioned parent tables (relkind = 'p').
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = number of days (int)
///   $3 = minimum table size in bytes (i64)
///   $4 = maximum table size in bytes (i64)
///   $5 = limit (i64, use i64::MAX for no limit)
pub const FIND_ANALYZE_OVERDUE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)          AS n_live_tup,
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze,
        EXTRACT(EPOCH FROM now() - GREATEST(t.last_analyze, t.last_autoanalyze))::float8
            / 86400.0 AS days_since_analyze
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND c.relkind != 'p'
      AND GREATEST(t.last_analyze, t.last_autoanalyze) < now() - make_interval(days => $2::int)
      AND pg_table_size(t.relid) BETWEEN $3 AND $4
    ORDER BY GREATEST(t.last_analyze, t.last_autoanalyze) ASC
    LIMIT $5;
"#;

/// Same as FIND_ANALYZE_OVERDUE but scoped to a single table.
/// Parameters:
///   $1 = array of schema names (text[])
///   $2 = table name (text)
///   $3 = number of days (int)
///   $4 = minimum table size in bytes (i64)
///   $5 = maximum table size in bytes (i64)
pub const FIND_ANALYZE_OVERDUE_TABLE: &str = r#"
    SELECT
        t.schemaname,
        t.relname AS tablename,
        COALESCE(t.n_live_tup, -1)          AS n_live_tup,
        COALESCE(t.n_mod_since_analyze, -1) AS n_mod_since_analyze,
        EXTRACT(EPOCH FROM now() - GREATEST(t.last_analyze, t.last_autoanalyze))::float8
            / 86400.0 AS days_since_analyze
    FROM pg_stat_user_tables t
    JOIN pg_class c ON c.oid = t.relid
    WHERE t.schemaname = ANY($1::text[])
      AND t.relname = $2
      AND c.relkind != 'p'
      AND GREATEST(t.last_analyze, t.last_autoanalyze) < now() - make_interval(days => $3::int)
      AND pg_table_size(t.relid) BETWEEN $4 AND $5
    ORDER BY GREATEST(t.last_analyze, t.last_autoanalyze) ASC;
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
      (schema_name, table_name, operation, mode, status,
       dead_tuples_before, dead_tuples_removed, duration_ms, error_message, run_started_at)
    VALUES
      ($1, $2, $3, $4, $5, $6, $7, $8, $9, now())
"#;

/// Every standby currently streaming from this primary, with its replay lag.
///
/// `replay_lag` is the round trip the primary observes: from flushing WAL locally
/// to the standby confirming it has replayed it. It is NULL when the standby is
/// caught up and there is no recent WAL to measure against, which the caller
/// treats as zero rather than as unknown.
///
/// An empty result means either "no replicas" or "this role cannot see the view";
/// pair it with GET_CAN_READ_REPLICATION_STATS to tell those apart.
///
/// Parameters: none
pub const GET_REPLICATION_LAG: &str = r#"
    SELECT
        COALESCE(application_name, '')          AS application_name,
        COALESCE(state, '')                     AS state,
        COALESCE(sync_state, '')                AS sync_state,
        EXTRACT(EPOCH FROM replay_lag)::float8  AS replay_lag_seconds
    FROM pg_stat_replication
    ORDER BY replay_lag DESC NULLS LAST;
"#;

/// Whether the connected role may read pg_stat_replication's contents.
///
/// Without membership of pg_read_all_stats (which pg_monitor and superuser both
/// confer) the view comes back empty, indistinguishable from a cluster with no
/// replicas at all.
///
/// Parameters: none
pub const GET_CAN_READ_REPLICATION_STATS: &str = r#"
    SELECT pg_has_role(current_user, 'pg_read_all_stats', 'USAGE') AS can_read;
"#;

/// Which of the explicitly requested schema/table pairs actually exist.
///
/// Takes the request as two parallel arrays so the whole --also-tables list costs
/// one round trip. Anything requested but not returned does not exist and is
/// reported to the operator.
///
/// Accepts ordinary tables, materialized views and partitioned parents. Discovery
/// excludes partitioned parents because their partitions are found individually,
/// but an explicitly named parent is a deliberate request and VACUUM cascades.
///
/// Parameters:
///   $1 = requested schema names (text[])
///   $2 = requested table names, positionally paired with $1 (text[])
pub const FIND_EXPLICIT_TABLES: &str = r#"
    SELECT n.nspname AS schema_name,
           c.relname AS table_name
    FROM unnest($1::text[], $2::text[]) AS req(schema_name, table_name)
    JOIN pg_namespace n ON n.nspname = req.schema_name
    JOIN pg_class     c ON c.relnamespace = n.oid AND c.relname = req.table_name
    WHERE c.relkind IN ('r', 'm', 'p');
"#;
