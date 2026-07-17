//! Unit tests for types, credential parsing, and SQL query constants.
//! These tests run without a PostgreSQL connection.

use pg_maintainer::credentials::get_password_from_pgpass;
use pg_maintainer::queries;
use pg_maintainer::types::{
    BloatTableInfo, FreezeTableInfo, LogFormat, Mode, OperationSummary, SslMode, TableInfo,
};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
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
