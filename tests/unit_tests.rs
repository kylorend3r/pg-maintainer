//! Unit tests for types, credential parsing, and SQL query constants.
//! These tests run without a PostgreSQL connection.

use pg_maintainer::config::{
    GENTLE_VACUUM_COST_DELAY_MS, GENTLE_VACUUM_COST_LIMIT, MAX_VACUUM_COST_DELAY_MS,
    MAX_VACUUM_COST_LIMIT, MIN_VACUUM_COST_LIMIT,
};
use pg_maintainer::credentials::get_password_from_pgpass;
use pg_maintainer::dsn::{self, ParsedDsn};
use pg_maintainer::queries;
use pg_maintainer::types::{
    BloatTableInfo, FreezeTableInfo, LogFormat, Mode, OperationSummary, SslMode, TableInfo,
    ThrottleSettings,
};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::str::FromStr;
use tempfile::Builder;

// ── SslMode ────────────────────────────────────────────────────────────────────

#[test]
fn test_sslmode_from_str_all_variants() {
    assert_eq!("disable".parse::<SslMode>().unwrap(), SslMode::Disable);
    assert_eq!("require".parse::<SslMode>().unwrap(), SslMode::Require);
    assert_eq!("verify-ca".parse::<SslMode>().unwrap(), SslMode::VerifyCa);
    assert_eq!(
        "verify-full".parse::<SslMode>().unwrap(),
        SslMode::VerifyFull
    );
}

#[test]
fn test_sslmode_from_str_case_insensitive() {
    assert_eq!("DISABLE".parse::<SslMode>().unwrap(), SslMode::Disable);
    assert_eq!("REQUIRE".parse::<SslMode>().unwrap(), SslMode::Require);
    assert_eq!("Verify-CA".parse::<SslMode>().unwrap(), SslMode::VerifyCa);
    assert_eq!(
        "VERIFY-FULL".parse::<SslMode>().unwrap(),
        SslMode::VerifyFull
    );
}

#[test]
fn test_sslmode_from_str_invalid() {
    assert!("bogus".parse::<SslMode>().is_err());
    assert!("".parse::<SslMode>().is_err());
    assert!("tls".parse::<SslMode>().is_err());
}

#[test]
fn test_sslmode_display() {
    assert_eq!(SslMode::Disable.to_string(), "disable");
    assert_eq!(SslMode::Require.to_string(), "require");
    assert_eq!(SslMode::VerifyCa.to_string(), "verify-ca");
    assert_eq!(SslMode::VerifyFull.to_string(), "verify-full");
}

#[test]
fn test_sslmode_default_is_disable() {
    assert_eq!(SslMode::default(), SslMode::Disable);
}

// ── LogFormat ──────────────────────────────────────────────────────────────────

#[test]
fn test_log_format_from_str() {
    assert_eq!("text".parse::<LogFormat>().unwrap(), LogFormat::Text);
    assert_eq!("json".parse::<LogFormat>().unwrap(), LogFormat::Json);
}

#[test]
fn test_log_format_from_str_case_insensitive() {
    assert_eq!("TEXT".parse::<LogFormat>().unwrap(), LogFormat::Text);
    assert_eq!("JSON".parse::<LogFormat>().unwrap(), LogFormat::Json);
    assert_eq!("Text".parse::<LogFormat>().unwrap(), LogFormat::Text);
    assert_eq!("Json".parse::<LogFormat>().unwrap(), LogFormat::Json);
}

#[test]
fn test_log_format_from_str_invalid() {
    assert!("xml".parse::<LogFormat>().is_err());
    assert!("".parse::<LogFormat>().is_err());
    assert!("csv".parse::<LogFormat>().is_err());
}

#[test]
fn test_log_format_display() {
    assert_eq!(LogFormat::Text.to_string(), "text");
    assert_eq!(LogFormat::Json.to_string(), "json");
}

#[test]
fn test_log_format_default_is_text() {
    assert_eq!(LogFormat::default(), LogFormat::Text);
}

// ── FreezeTableInfo ────────────────────────────────────────────────────────────

#[test]
fn test_pct_toward_wraparound_normal() {
    let info = FreezeTableInfo {
        schema_name: "public".into(),
        table_name: "orders".into(),
        xid_age: 100_000_000,
        freeze_max_age: 200_000_000,
    };
    assert!((info.pct_toward_wraparound() - 50.0).abs() < 0.001);
}

#[test]
fn test_pct_toward_wraparound_at_threshold() {
    let info = FreezeTableInfo {
        schema_name: "public".into(),
        table_name: "t".into(),
        xid_age: 200_000_000,
        freeze_max_age: 200_000_000,
    };
    assert!((info.pct_toward_wraparound() - 100.0).abs() < 0.001);
}

#[test]
fn test_pct_toward_wraparound_exceeds_threshold() {
    let info = FreezeTableInfo {
        schema_name: "public".into(),
        table_name: "t".into(),
        xid_age: 300_000_000,
        freeze_max_age: 200_000_000,
    };
    assert!(info.pct_toward_wraparound() > 100.0);
}

#[test]
fn test_pct_toward_wraparound_zero_max_age() {
    let info = FreezeTableInfo {
        schema_name: "public".into(),
        table_name: "t".into(),
        xid_age: 1,
        freeze_max_age: 0,
    };
    // Division by zero guard: returns 100.0
    assert!((info.pct_toward_wraparound() - 100.0).abs() < 0.001);
}

// ── TableInfo ──────────────────────────────────────────────────────────────────

#[test]
fn test_table_info_construction() {
    let t = TableInfo {
        schema_name: "public".into(),
        table_name: "users".into(),
        n_live_tup: 50_000,
        n_dead_tup: 200,
    };
    assert_eq!(t.schema_name, "public");
    assert_eq!(t.table_name, "users");
    assert_eq!(t.n_live_tup, 50_000);
    assert_eq!(t.n_dead_tup, 200);
}

// ── OperationSummary ───────────────────────────────────────────────────────────

#[test]
fn test_operation_summary_default_is_zero() {
    let s = OperationSummary::default();
    assert_eq!(s.total, 0);
    assert_eq!(s.succeeded, 0);
    assert_eq!(s.failed, 0);
    assert_eq!(s.skipped, 0);
}

// ── SQL query constants ────────────────────────────────────────────────────────

#[test]
fn test_find_never_vacuumed_uses_any_cast() {
    assert!(
        queries::FIND_NEVER_VACUUMED.contains("= ANY($1::text[])"),
        "FIND_NEVER_VACUUMED must use explicit ::text[] cast"
    );
}

#[test]
fn test_find_never_vacuumed_table_uses_any_cast() {
    assert!(
        queries::FIND_NEVER_VACUUMED_TABLE.contains("= ANY($1::text[])"),
        "FIND_NEVER_VACUUMED_TABLE must use explicit ::text[] cast"
    );
    assert!(
        queries::FIND_NEVER_VACUUMED_TABLE.contains("$2"),
        "FIND_NEVER_VACUUMED_TABLE must bind table name as $2"
    );
}

#[test]
fn test_find_never_analyzed_uses_any_cast() {
    assert!(
        queries::FIND_NEVER_ANALYZED.contains("= ANY($1::text[])"),
        "FIND_NEVER_ANALYZED must use explicit ::text[] cast"
    );
}

#[test]
fn test_find_never_analyzed_table_uses_any_cast() {
    assert!(
        queries::FIND_NEVER_ANALYZED_TABLE.contains("= ANY($1::text[])"),
        "FIND_NEVER_ANALYZED_TABLE must use explicit ::text[] cast"
    );
    assert!(
        queries::FIND_NEVER_ANALYZED_TABLE.contains("$2"),
        "FIND_NEVER_ANALYZED_TABLE must bind table name as $2"
    );
}

#[test]
fn test_find_wraparound_candidates_uses_any_cast() {
    assert!(
        queries::FIND_WRAPAROUND_CANDIDATES.contains("= ANY($1::text[])"),
        "FIND_WRAPAROUND_CANDIDATES must use explicit ::text[] cast"
    );
}

#[test]
fn test_find_wraparound_candidates_table_uses_any_cast() {
    assert!(
        queries::FIND_WRAPAROUND_CANDIDATES_TABLE.contains("= ANY($1::text[])"),
        "FIND_WRAPAROUND_CANDIDATES_TABLE must use explicit ::text[] cast"
    );
    assert!(
        queries::FIND_WRAPAROUND_CANDIDATES_TABLE.contains("$3"),
        "FIND_WRAPAROUND_CANDIDATES_TABLE must bind table name as $3"
    );
}

#[test]
fn test_partition_excluding_queries_all_filter_relkind_p() {
    let queries_to_check = [
        ("FIND_NEVER_VACUUMED", queries::FIND_NEVER_VACUUMED),
        (
            "FIND_NEVER_VACUUMED_TABLE",
            queries::FIND_NEVER_VACUUMED_TABLE,
        ),
        ("FIND_NEVER_ANALYZED", queries::FIND_NEVER_ANALYZED),
        (
            "FIND_NEVER_ANALYZED_TABLE",
            queries::FIND_NEVER_ANALYZED_TABLE,
        ),
        ("FIND_BLOAT_CANDIDATES", queries::FIND_BLOAT_CANDIDATES),
        (
            "FIND_BLOAT_CANDIDATES_TABLE",
            queries::FIND_BLOAT_CANDIDATES_TABLE,
        ),
        ("FIND_STALE_STATS", queries::FIND_STALE_STATS),
        ("FIND_STALE_STATS_TABLE", queries::FIND_STALE_STATS_TABLE),
    ];
    for (name, sql) in &queries_to_check {
        assert!(
            sql.contains("relkind != 'p'"),
            "{name} must exclude partitioned parent tables (relkind != 'p')"
        );
    }
}

// ── Mode ───────────────────────────────────────────────────────────────────────

#[test]
fn test_mode_from_str_all_variants() {
    assert_eq!(
        "never-vacuumed".parse::<Mode>().unwrap(),
        Mode::NeverVacuumed
    );
    assert_eq!(
        "never-analyzed".parse::<Mode>().unwrap(),
        Mode::NeverAnalyzed
    );
    assert_eq!("wraparound".parse::<Mode>().unwrap(), Mode::Wraparound);
    assert_eq!("bloated".parse::<Mode>().unwrap(), Mode::Bloated);
    assert_eq!("stale-stats".parse::<Mode>().unwrap(), Mode::StaleStats);
}

#[test]
fn test_mode_from_str_case_insensitive() {
    assert_eq!(
        "NEVER-VACUUMED".parse::<Mode>().unwrap(),
        Mode::NeverVacuumed
    );
    assert_eq!(
        "Never-Analyzed".parse::<Mode>().unwrap(),
        Mode::NeverAnalyzed
    );
    assert_eq!("WRAPAROUND".parse::<Mode>().unwrap(), Mode::Wraparound);
    assert_eq!("Bloated".parse::<Mode>().unwrap(), Mode::Bloated);
}

#[test]
fn test_mode_from_str_invalid() {
    assert!("invalid".parse::<Mode>().is_err());
    assert!("".parse::<Mode>().is_err());
    assert!("vac".parse::<Mode>().is_err());
    assert!("vacuum".parse::<Mode>().is_err()); // old names no longer accepted
}

#[test]
fn test_mode_display() {
    assert_eq!(Mode::NeverVacuumed.to_string(), "never-vacuumed");
    assert_eq!(Mode::NeverAnalyzed.to_string(), "never-analyzed");
    assert_eq!(Mode::Wraparound.to_string(), "wraparound");
    assert_eq!(Mode::Bloated.to_string(), "bloated");
    assert_eq!(Mode::StaleStats.to_string(), "stale-stats");
}

// ── BloatTableInfo ────────────────────────────────────────────────────────────

#[test]
fn test_bloat_table_info_pct_bloat_full_bloat() {
    let info = BloatTableInfo {
        schema_name: "public".to_string(),
        table_name: "test".to_string(),
        n_live_tup: 100,
        n_dead_tup: 400, // 80% bloat
    };
    assert_eq!(info.pct_bloat(), 80.0);
}

#[test]
fn test_bloat_table_info_pct_bloat_zero() {
    let info = BloatTableInfo {
        schema_name: "public".to_string(),
        table_name: "test".to_string(),
        n_live_tup: 100,
        n_dead_tup: 0,
    };
    assert_eq!(info.pct_bloat(), 0.0);
}

#[test]
fn test_bloat_table_info_pct_bloat_100() {
    let info = BloatTableInfo {
        schema_name: "public".to_string(),
        table_name: "test".to_string(),
        n_live_tup: 0,
        n_dead_tup: 100,
    };
    assert_eq!(info.pct_bloat(), 100.0);
}

#[test]
fn test_bloat_table_info_pct_bloat_empty_table() {
    let info = BloatTableInfo {
        schema_name: "public".to_string(),
        table_name: "test".to_string(),
        n_live_tup: 0,
        n_dead_tup: 0, // Empty table
    };
    assert_eq!(info.pct_bloat(), 0.0);
}

#[test]
fn test_wraparound_query_includes_toast_tables() {
    assert!(
        queries::FIND_WRAPAROUND_CANDIDATES.contains("'t'"),
        "wraparound query must include TOAST tables (relkind 't')"
    );
}

#[test]
fn test_wraparound_query_includes_materialized_views() {
    assert!(
        queries::FIND_WRAPAROUND_CANDIDATES.contains("'m'"),
        "wraparound query must include materialized views (relkind 'm')"
    );
}

#[test]
fn test_wraparound_query_excludes_system_schemas() {
    assert!(
        queries::FIND_WRAPAROUND_CANDIDATES.contains("'pg_catalog'"),
        "wraparound query must exclude pg_catalog"
    );
    assert!(
        queries::FIND_WRAPAROUND_CANDIDATES.contains("'information_schema'"),
        "wraparound query must exclude information_schema"
    );
}

// ── ThrottleSettings ──────────────────────────────────────────────────────────

#[test]
fn test_throttle_unset_by_default() {
    let t = ThrottleSettings::resolve(None, None, false);
    assert_eq!(t.cost_delay_ms, None);
    assert_eq!(t.cost_limit, None);
    assert!(!t.is_enabled());
    assert!(!t.is_active());
}

#[test]
fn test_throttle_gentle_fills_both_values() {
    let t = ThrottleSettings::resolve(None, None, true);
    assert_eq!(t.cost_delay_ms, Some(GENTLE_VACUUM_COST_DELAY_MS));
    assert_eq!(t.cost_limit, Some(GENTLE_VACUUM_COST_LIMIT));
    assert!(t.is_enabled());
    assert!(t.is_active());
}

#[test]
fn test_throttle_explicit_delay_wins_over_gentle() {
    let t = ThrottleSettings::resolve(Some(25.0), None, true);
    assert_eq!(t.cost_delay_ms, Some(25.0));
    // the limit still comes from the preset
    assert_eq!(t.cost_limit, Some(GENTLE_VACUUM_COST_LIMIT));
}

#[test]
fn test_throttle_explicit_limit_wins_over_gentle() {
    let t = ThrottleSettings::resolve(None, Some(400), true);
    assert_eq!(t.cost_delay_ms, Some(GENTLE_VACUUM_COST_DELAY_MS));
    assert_eq!(t.cost_limit, Some(400));
}

#[test]
fn test_throttle_both_explicit_ignore_gentle() {
    let t = ThrottleSettings::resolve(Some(5.0), Some(1000), true);
    assert_eq!(t.cost_delay_ms, Some(5.0));
    assert_eq!(t.cost_limit, Some(1000));
}

#[test]
fn test_throttle_explicit_without_gentle() {
    let t = ThrottleSettings::resolve(Some(20.0), Some(300), false);
    assert_eq!(t.cost_delay_ms, Some(20.0));
    assert_eq!(t.cost_limit, Some(300));
}

#[test]
fn test_throttle_limit_alone_is_enabled_but_not_active() {
    // A cost limit without a delay changes nothing about the I/O rate,
    // but the SET is still issued.
    let t = ThrottleSettings::resolve(None, Some(500), false);
    assert!(t.is_enabled());
    assert!(!t.is_active());
}

#[test]
fn test_throttle_zero_delay_is_explicit_disable() {
    // 0 is a valid, meaningful value: it overrides a server-configured delay.
    let t = ThrottleSettings::resolve(Some(0.0), None, false);
    assert!(t.validate().is_ok());
    assert!(t.is_enabled(), "an explicit 0 must still issue the SET");
    assert!(!t.is_active(), "a 0 delay does not throttle anything");
}

#[test]
fn test_throttle_validate_delay_boundaries() {
    assert!(
        ThrottleSettings::resolve(Some(0.0), None, false)
            .validate()
            .is_ok()
    );
    assert!(
        ThrottleSettings::resolve(Some(MAX_VACUUM_COST_DELAY_MS), None, false)
            .validate()
            .is_ok()
    );
    assert!(
        ThrottleSettings::resolve(Some(-1.0), None, false)
            .validate()
            .is_err()
    );
    assert!(
        ThrottleSettings::resolve(Some(MAX_VACUUM_COST_DELAY_MS + 0.1), None, false)
            .validate()
            .is_err()
    );
}

#[test]
fn test_throttle_validate_limit_boundaries() {
    assert!(
        ThrottleSettings::resolve(None, Some(MIN_VACUUM_COST_LIMIT), false)
            .validate()
            .is_ok()
    );
    assert!(
        ThrottleSettings::resolve(None, Some(MAX_VACUUM_COST_LIMIT), false)
            .validate()
            .is_ok()
    );
    // PostgreSQL's vacuum_cost_limit floor is 1, not 0
    assert!(
        ThrottleSettings::resolve(None, Some(0), false)
            .validate()
            .is_err()
    );
    assert!(
        ThrottleSettings::resolve(None, Some(MAX_VACUUM_COST_LIMIT + 1), false)
            .validate()
            .is_err()
    );
    assert!(
        ThrottleSettings::resolve(None, Some(-5), false)
            .validate()
            .is_err()
    );
}

#[test]
fn test_throttle_validate_error_names_the_flag() {
    let delay_err = ThrottleSettings::resolve(Some(500.0), None, false)
        .validate()
        .unwrap_err();
    assert!(delay_err.contains("--vacuum-cost-delay-ms"), "{delay_err}");

    let limit_err = ThrottleSettings::resolve(None, Some(99_999), false)
        .validate()
        .unwrap_err();
    assert!(limit_err.contains("--vacuum-cost-limit"), "{limit_err}");
}

#[test]
fn test_throttle_gentle_preset_is_within_postgres_ranges() {
    assert!(
        ThrottleSettings::resolve(None, None, true)
            .validate()
            .is_ok()
    );
}

// ── Pgpass credential lookup ───────────────────────────────────────────────────
//
// env::set_var / remove_var are unsafe in Rust 2024 (mutating global env state in
// a multi-threaded context is unsound). A process-wide mutex serializes all pgpass
// tests so they cannot race on the PGPASSFILE environment variable.

static PGPASS_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_pgpass_file<F: FnOnce() -> R, R>(path: &std::path::Path, f: F) -> R {
    let _guard = PGPASS_MUTEX.lock().unwrap();
    // Safety: PGPASS_MUTEX ensures exclusive access to PGPASSFILE in this process.
    unsafe { std::env::set_var("PGPASSFILE", path.to_str().unwrap()) };
    let result = f();
    unsafe { std::env::remove_var("PGPASSFILE") };
    result
}

#[test]
fn test_pgpass_no_file_returns_none() {
    let nonexistent = std::path::Path::new("/nonexistent/.pgpass");
    let result = with_pgpass_file(nonexistent, || {
        get_password_from_pgpass("localhost", 5432, "mydb", "myuser")
    });
    assert!(result.unwrap().is_none());
}

#[test]
fn test_pgpass_exact_match() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(f.as_file_mut(), "myhost:5432:mydb:myuser:s3cr3t").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("myhost", 5432, "mydb", "myuser").unwrap()
    });
    assert_eq!(password, Some("s3cr3t".to_string()));
}

#[test]
fn test_pgpass_wildcard_host_matches() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(f.as_file_mut(), "*:5432:mydb:myuser:wildpass").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("anyhost", 5432, "mydb", "myuser").unwrap()
    });
    assert_eq!(password, Some("wildpass".to_string()));
}

#[test]
fn test_pgpass_wildcard_all_fields() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(f.as_file_mut(), "*:*:*:*:globalpass").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("host", 9999, "db", "user").unwrap()
    });
    assert_eq!(password, Some("globalpass".to_string()));
}

#[test]
fn test_pgpass_no_matching_entry() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(f.as_file_mut(), "otherhost:5432:otherdb:otheruser:pass").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("myhost", 5432, "mydb", "myuser").unwrap()
    });
    assert!(password.is_none());
}

#[test]
fn test_pgpass_comments_and_blank_lines_ignored() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(
        f.as_file_mut(),
        "# this is a comment\n\nmyhost:5432:mydb:myuser:commentpass"
    )
    .unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("myhost", 5432, "mydb", "myuser").unwrap()
    });
    assert_eq!(password, Some("commentpass".to_string()));
}

#[test]
fn test_pgpass_wrong_permissions_returns_none() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(f.as_file_mut(), "myhost:5432:mydb:myuser:s3cr3t").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("myhost", 5432, "mydb", "myuser").unwrap()
    });
    // Wrong permissions → ignored, returns None
    assert!(password.is_none());
}

#[test]
fn test_pgpass_first_match_wins() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(
        f.as_file_mut(),
        "myhost:5432:mydb:myuser:first\nmyhost:5432:mydb:myuser:second"
    )
    .unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("myhost", 5432, "mydb", "myuser").unwrap()
    });
    assert_eq!(password, Some("first".to_string()));
}

#[test]
fn test_pgpass_escaped_colon_in_password() {
    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    // Password contains a colon escaped with backslash
    writeln!(f.as_file_mut(), r"myhost:5432:mydb:myuser:pass\:word").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    let password = with_pgpass_file(f.path(), || {
        get_password_from_pgpass("myhost", 5432, "mydb", "myuser").unwrap()
    });
    assert_eq!(password, Some("pass:word".to_string()));
}

// ── DSN parsing ───────────────────────────────────────────────────────────────

fn parse_ok(s: &str) -> ParsedDsn {
    dsn::parse(s).unwrap_or_else(|e| panic!("expected {s:?} to parse, got: {e}"))
}

#[test]
fn test_dsn_full_uri_all_fields() {
    let d = parse_ok("postgres://alice:s3cr3t@db.host:5433/mydb");
    assert_eq!(d.host.as_deref(), Some("db.host"));
    assert_eq!(d.port, Some(5433));
    assert_eq!(d.database.as_deref(), Some("mydb"));
    assert_eq!(d.username.as_deref(), Some("alice"));
    assert_eq!(d.password.as_deref(), Some("s3cr3t"));
}

#[test]
fn test_dsn_postgresql_scheme_also_accepted() {
    let d = parse_ok("postgresql://alice@db.host/mydb");
    assert_eq!(d.host.as_deref(), Some("db.host"));
    assert_eq!(d.database.as_deref(), Some("mydb"));
    assert_eq!(d.password, None);
}

#[test]
fn test_dsn_keyword_string_all_fields() {
    let d = parse_ok("host=db.host port=5433 dbname=mydb user=alice password=s3cr3t");
    assert_eq!(d.host.as_deref(), Some("db.host"));
    assert_eq!(d.port, Some(5433));
    assert_eq!(d.database.as_deref(), Some("mydb"));
    assert_eq!(d.username.as_deref(), Some("alice"));
    assert_eq!(d.password.as_deref(), Some("s3cr3t"));
}

#[test]
fn test_dsn_partial_uri_leaves_rest_unset() {
    // Only a host: everything else must stay None so other sources can fill in.
    let d = parse_ok("postgres://db.host");
    assert_eq!(d.host.as_deref(), Some("db.host"));
    assert_eq!(d.database, None);
    assert_eq!(d.username, None);
    assert_eq!(d.password, None);
    assert_eq!(d.sslmode, None);
}

// ── Port presence ─────────────────────────────────────────────────────────────

#[test]
fn test_dsn_uri_without_port_reports_none() {
    // The URI parser substitutes 5432; we must not mistake that for an explicit
    // port, or a DSN would silently override PG_PORT.
    assert_eq!(parse_ok("postgres://db.host/mydb").port, None);
}

#[test]
fn test_dsn_uri_with_explicit_default_port_reports_it() {
    assert_eq!(parse_ok("postgres://db.host:5432/mydb").port, Some(5432));
}

#[test]
fn test_dsn_keyword_without_port_reports_none() {
    assert_eq!(parse_ok("host=db.host dbname=mydb").port, None);
}

#[test]
fn test_dsn_port_with_userinfo_containing_no_port() {
    assert_eq!(parse_ok("postgres://alice:pw@db.host/mydb").port, None);
    assert_eq!(
        parse_ok("postgres://alice:pw@db.host:6000/mydb").port,
        Some(6000)
    );
}

#[test]
fn test_dsn_ipv6_host_port_detection() {
    let with_port = parse_ok("postgres://u@[::1]:5433/mydb");
    assert_eq!(with_port.host.as_deref(), Some("::1"));
    assert_eq!(with_port.port, Some(5433));

    let without = parse_ok("postgres://u@[::1]/mydb");
    assert_eq!(without.host.as_deref(), Some("::1"));
    assert_eq!(without.port, None, "IPv6 colons must not read as a port");
}

// ── Passwords ─────────────────────────────────────────────────────────────────

#[test]
fn test_dsn_percent_encoded_password() {
    let d = parse_ok("postgres://alice:p%40ss%2Fword@db.host/mydb");
    assert_eq!(d.password.as_deref(), Some("p@ss/word"));
    assert_eq!(d.host.as_deref(), Some("db.host"));
}

#[test]
fn test_dsn_keyword_quoted_password_with_space() {
    let d = parse_ok("host=h dbname=d password='pass word'");
    assert_eq!(d.password.as_deref(), Some("pass word"));
}

#[test]
fn test_dsn_keyword_escaped_quote_in_password() {
    let d = parse_ok(r"host=h password='pa\'ss'");
    assert_eq!(d.password.as_deref(), Some("pa'ss"));
}

#[test]
fn test_dsn_keyword_escaped_backslash_in_password() {
    let d = parse_ok(r"host=h password='pa\\ss'");
    assert_eq!(d.password.as_deref(), Some(r"pa\ss"));
}

// ── SSL parameters ────────────────────────────────────────────────────────────

#[test]
fn test_dsn_sslmode_all_supported_values() {
    for (text, expected) in [
        ("disable", SslMode::Disable),
        ("require", SslMode::Require),
        ("verify-ca", SslMode::VerifyCa),
        ("verify-full", SslMode::VerifyFull),
    ] {
        let d = parse_ok(&format!("postgres://h/db?sslmode={text}"));
        assert_eq!(d.sslmode, Some(expected), "sslmode={text}");
    }
}

#[test]
fn test_dsn_sslmode_verify_full_would_break_tokio_postgres_alone() {
    // The whole reason SSL params are extracted before tokio-postgres sees them.
    assert!(
        tokio_postgres::Config::from_str("postgres://h/db?sslmode=verify-full").is_err(),
        "if tokio-postgres ever accepts this, the pre-processing can be simplified"
    );
    assert_eq!(
        parse_ok("postgres://h/db?sslmode=verify-full").sslmode,
        Some(SslMode::VerifyFull)
    );
}

#[test]
fn test_dsn_sslmode_in_keyword_form() {
    let d = parse_ok("host=h dbname=d sslmode=verify-ca");
    assert_eq!(d.sslmode, Some(SslMode::VerifyCa));
    assert_eq!(d.host.as_deref(), Some("h"));
}

#[test]
fn test_dsn_sslmode_prefer_and_allow_rejected_by_name() {
    for mode in ["prefer", "allow"] {
        let err = dsn::parse(&format!("postgres://h/db?sslmode={mode}"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(mode), "{err}");
        assert!(
            err.contains("verify-full"),
            "must name what is supported: {err}"
        );
    }
}

#[test]
fn test_dsn_sslmode_unknown_rejected() {
    assert!(dsn::parse("postgres://h/db?sslmode=banana").is_err());
}

#[test]
fn test_dsn_certificate_paths_extracted() {
    let d = parse_ok(
        "postgres://h/db?sslmode=verify-full&sslrootcert=/etc/ca.pem\
         &sslcert=/etc/client.pem&sslkey=/etc/client.key",
    );
    assert_eq!(d.sslmode, Some(SslMode::VerifyFull));
    assert_eq!(d.ssl_ca_cert.as_deref(), Some("/etc/ca.pem"));
    assert_eq!(d.ssl_client_cert.as_deref(), Some("/etc/client.pem"));
    assert_eq!(d.ssl_client_key.as_deref(), Some("/etc/client.key"));
    assert_eq!(d.host.as_deref(), Some("h"));
    assert_eq!(d.database.as_deref(), Some("db"));
}

#[test]
fn test_dsn_certificate_paths_in_keyword_form() {
    let d = parse_ok("host=h dbname=db sslrootcert=/etc/ca.pem user=alice");
    assert_eq!(d.ssl_ca_cert.as_deref(), Some("/etc/ca.pem"));
    assert_eq!(d.username.as_deref(), Some("alice"));
}

#[test]
fn test_dsn_percent_encoded_cert_path() {
    let d = parse_ok("postgres://h/db?sslrootcert=%2Fetc%2Fmy%20certs%2Fca.pem");
    assert_eq!(d.ssl_ca_cert.as_deref(), Some("/etc/my certs/ca.pem"));
}

#[test]
fn test_dsn_non_ssl_params_survive_extraction() {
    // Stripping the SSL params must not disturb the rest of the query string.
    let d = parse_ok("postgres://h/db?sslmode=require&connect_timeout=7");
    assert_eq!(d.sslmode, Some(SslMode::Require));
    assert_eq!(d.connect_timeout_seconds, Some(7));
}

#[test]
fn test_dsn_connect_timeout_extracted() {
    assert_eq!(
        parse_ok("postgres://h/db?connect_timeout=25").connect_timeout_seconds,
        Some(25)
    );
    assert_eq!(parse_ok("postgres://h/db").connect_timeout_seconds, None);
}

// ── Ignored parameters ────────────────────────────────────────────────────────

#[test]
fn test_dsn_reports_ignored_params() {
    let d = parse_ok("postgres://h/db?application_name=zzz&target_session_attrs=read-write");
    assert!(d.ignored_params.contains(&"application_name".to_string()));
    assert!(
        d.ignored_params
            .contains(&"target_session_attrs".to_string())
    );
}

#[test]
fn test_dsn_used_params_are_not_reported_as_ignored() {
    let d = parse_ok("postgres://h/db?connect_timeout=7&sslmode=require");
    assert!(d.ignored_params.is_empty(), "{:?}", d.ignored_params);
}

// ── Rejections ────────────────────────────────────────────────────────────────

#[test]
fn test_dsn_multi_host_rejected() {
    let err = dsn::parse("postgres://u@h1:5432,h2:5433/db")
        .unwrap_err()
        .to_string();
    assert!(err.contains("single host"), "{err}");
}

#[test]
fn test_dsn_multi_host_rejected_keyword_form() {
    assert!(dsn::parse("host=h1,h2 dbname=db").is_err());
}

#[test]
fn test_dsn_garbage_rejected() {
    assert!(dsn::parse("not-a-dsn").is_err());
    assert!(dsn::parse("http://h/db").is_err());
}

#[test]
fn test_dsn_empty_rejected() {
    assert!(dsn::parse("").is_err());
    assert!(dsn::parse("   ").is_err());
}

#[test]
fn test_dsn_unterminated_quote_rejected() {
    let err = dsn::parse("host=h password='unclosed")
        .unwrap_err()
        .to_string();
    assert!(err.contains("unterminated"), "{err}");
}

#[test]
fn test_dsn_keyword_missing_value_rejected() {
    assert!(dsn::parse("host=h dbname").is_err());
}

#[test]
fn test_dsn_unknown_param_rejected() {
    // tokio-postgres rejects these, and the error should reach the operator.
    assert!(dsn::parse("postgres://h/db?bogus_param=1").is_err());
}

// ── Unix sockets ──────────────────────────────────────────────────────────────

#[test]
fn test_dsn_unix_socket_host() {
    let d = parse_ok("postgres:///mydb?host=/var/run/postgresql");
    assert_eq!(d.host.as_deref(), Some("/var/run/postgresql"));
    assert_eq!(d.database.as_deref(), Some("mydb"));
}

// ── Redaction ─────────────────────────────────────────────────────────────────

#[test]
fn test_redact_uri_password() {
    let out = dsn::redact("postgres://alice:s3cr3t@db.host:5433/mydb");
    assert!(!out.contains("s3cr3t"), "{out}");
    assert!(out.contains("alice"), "{out}");
    assert!(out.contains("db.host"), "{out}");
    assert!(out.contains("mydb"), "{out}");
}

#[test]
fn test_redact_uri_without_password_is_unchanged() {
    let raw = "postgres://alice@db.host/mydb";
    assert_eq!(dsn::redact(raw), raw);
}

#[test]
fn test_redact_uri_without_userinfo_is_unchanged() {
    let raw = "postgres://db.host:5433/mydb";
    assert_eq!(dsn::redact(raw), raw);
}

#[test]
fn test_redact_keyword_password() {
    let out = dsn::redact("host=h dbname=d password=s3cr3t user=alice");
    assert!(!out.contains("s3cr3t"), "{out}");
    assert!(out.contains("host=h"), "{out}");
    assert!(out.contains("user=alice"), "{out}");
}

#[test]
fn test_redact_keyword_quoted_password() {
    let out = dsn::redact("host=h password='se cret'");
    assert!(!out.contains("se cret"), "{out}");
}

#[test]
fn test_redact_percent_encoded_password_leaves_nothing_behind() {
    let out = dsn::redact("postgres://alice:p%40ss@db.host/mydb");
    assert!(!out.contains("p%40ss"), "{out}");
    assert!(!out.contains("p@ss"), "{out}");
}

#[test]
fn test_redact_unparseable_falls_back_to_full_redaction() {
    assert_eq!(dsn::redact("host=h password='unclosed"), "[REDACTED]");
}
