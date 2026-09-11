//! CLI-level integration tests using assert_cmd.
//! Validates argument parsing, schema requirements, and pre-connection error paths.

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;
use tempfile::Builder;

fn cmd() -> Command {
    Command::cargo_bin("pg-maintainer").unwrap()
}

// ── Help / version ─────────────────────────────────────────────────────────────

#[test]
fn test_help_flag() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("pg-maintainer"))
        .stdout(predicate::str::contains("--schema"))
        .stdout(predicate::str::contains("--dry-run"));
}

#[test]
fn test_version_flag() {
    cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("pg-maintainer"))
        .stdout(predicate::str::contains("1.0.0"));
}

// ── Required-schema validation ─────────────────────────────────────────────────

#[test]
fn test_missing_schema_and_discover_all() {
    // Neither --schema nor --discover-all-schemas → runtime validation error
    cmd()
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Either --schema or --discover-all-schemas",
        ));
}

#[test]
fn test_schema_flag_accepted() {
    cmd()
        .arg("--schema")
        .arg("public")
        .env_clear()
        .assert()
        // Fails at DB connection, not at argument parsing (code 2) or schema validation (code 1 with schema message)
        .code(predicate::ne(2));
}

#[test]
fn test_discover_all_schemas_accepted() {
    cmd()
        .arg("--discover-all-schemas")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_discover_all_schemas_without_schema() {
    // --discover-all-schemas alone satisfies schema requirement
    cmd()
        .arg("--discover-all-schemas")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_help_contains_discover_all_schemas() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--discover-all-schemas"));
}

// ── Maintenance work mem validation ────────────────────────────────────────────

#[test]
fn test_maintenance_work_mem_too_high() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--maintenance-work-mem-gb")
        .arg("50")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("exceeds maximum"));
}

#[test]
fn test_maintenance_work_mem_at_max_boundary() {
    // 32 GB is the max — should pass argument parsing, fail only at DB connection
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--maintenance-work-mem-gb")
        .arg("32")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_maintenance_work_mem_default() {
    cmd()
        .arg("--schema")
        .arg("public")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Mode selection ────────────────────────────────────────────────────────────

#[test]
fn test_mode_single_never_vacuumed() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--mode")
        .arg("never-vacuumed")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Invalid mode").not());
}

#[test]
fn test_mode_multiple_comma_separated() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--mode")
        .arg("never-vacuumed,never-analyzed,wraparound")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Invalid mode").not());
}

#[test]
fn test_mode_all_five() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--mode")
        .arg("never-vacuumed,never-analyzed,wraparound,bloated,stale-stats")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Invalid mode").not());
}

#[test]
fn test_mode_invalid_name() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--mode")
        .arg("invalid")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid mode"));
}

#[test]
fn test_mode_case_insensitive() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--mode")
        .arg("NEVER-VACUUMED,Never-Analyzed,WRAPAROUND,bloated")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Invalid mode").not());
}

#[test]
fn test_mode_default_when_omitted() {
    // When --mode is omitted, all four modes should run (no error)
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Table filter ───────────────────────────────────────────────────────────────

#[test]
fn test_table_flag() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--table")
        .arg("orders")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Dry run ────────────────────────────────────────────────────────────────────

#[test]
fn test_dry_run_flag_parsing() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Wraparound min age ─────────────────────────────────────────────────────────

#[test]
fn test_wraparound_min_age_custom() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-min-age")
        .arg("100000000")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_wraparound_min_age_invalid() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-min-age")
        .arg("not_a_number")
        .assert()
        .failure()
        .code(2);
}

// ── SSL mode ───────────────────────────────────────────────────────────────────

#[test]
fn test_sslmode_disable() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--sslmode")
        .arg("disable")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_sslmode_require() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--sslmode")
        .arg("require")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_sslmode_verify_ca() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--sslmode")
        .arg("verify-ca")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_sslmode_verify_full() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--sslmode")
        .arg("verify-full")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_sslmode_invalid() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--sslmode")
        .arg("bogus")
        .assert()
        .failure()
        .code(2);
}

// ── Log format ─────────────────────────────────────────────────────────────────

#[test]
fn test_log_format_text() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--log-format")
        .arg("text")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_log_format_json() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--log-format")
        .arg("json")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_log_format_case_insensitive() {
    for fmt in &["TEXT", "text", "Text", "JSON", "json", "Json"] {
        cmd()
            .arg("--schema")
            .arg("public")
            .arg("--log-format")
            .arg(fmt)
            .env_clear()
            .assert()
            .code(predicate::ne(2));
    }
}

#[test]
fn test_log_format_invalid() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--log-format")
        .arg("xml")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_help_contains_log_format() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--log-format"));
}

// ── Silence mode ───────────────────────────────────────────────────────────────

#[test]
fn test_silence_mode_flag() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--silence-mode")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Connection flags ───────────────────────────────────────────────────────────

#[test]
fn test_connection_flags() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--host")
        .arg("localhost")
        .arg("--port")
        .arg("5432")
        .arg("--database")
        .arg("testdb")
        .arg("--username")
        .arg("testuser")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_port_invalid() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--port")
        .arg("not_a_port")
        .assert()
        .failure()
        .code(2);
}

// ── Password security ──────────────────────────────────────────────────────────

#[test]
fn test_password_cli_flag_emits_warning() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--password")
        .arg("secret")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Warning").and(predicate::str::contains("insecure")));
}

#[test]
fn test_no_insecure_warning_without_password_flag() {
    cmd()
        .arg("--schema")
        .arg("public")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("insecure").not());
}

// ── Config file ────────────────────────────────────────────────────────────────

#[test]
fn test_config_file_not_found() {
    cmd()
        .arg("--config")
        .arg("/nonexistent/config.toml")
        .env_clear()
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Configuration file not found")
                .or(predicate::str::contains("not found")),
        );
}

#[test]
fn test_config_file_invalid_toml() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(f.as_file_mut(), "invalid = toml = syntax").unwrap();

    cmd()
        .arg("--config")
        .arg(f.path().to_str().unwrap())
        .env_clear()
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("Failed to parse TOML").or(predicate::str::contains("TOML")),
        );
}

#[test]
fn test_config_file_valid_with_schema() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(
        f.as_file_mut(),
        r#"
host = "localhost"
port = 5432
database = "postgres"
username = "postgres"
schema = "public"
dry-run = false
"#
    )
    .unwrap();

    cmd()
        .arg("--config")
        .arg(f.path().to_str().unwrap())
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_config_file_cli_overrides_schema() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(
        f.as_file_mut(),
        r#"
host = "config-host"
schema = "analytics"
maintenance-work-mem-gb = 4
"#
    )
    .unwrap();

    // CLI --schema overrides config file schema
    cmd()
        .arg("--config")
        .arg(f.path().to_str().unwrap())
        .arg("--schema")
        .arg("public")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_config_file_with_env_interpolation() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(
        f.as_file_mut(),
        r#"
schema = "public"
password = "${{PG_TEST_SECRET}}"
"#
    )
    .unwrap();

    cmd()
        .arg("--config")
        .arg(f.path().to_str().unwrap())
        .env("PG_TEST_SECRET", "hunter2")
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_help_contains_config_option() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--config").and(predicate::str::contains("TOML")));
}

// ── pgpass permission warning ──────────────────────────────────────────────────

#[cfg(unix)]
#[test]
fn test_pgpass_wrong_permissions_emits_warning() {
    use std::os::unix::fs::PermissionsExt;

    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(f.as_file_mut(), "localhost:5432:mydb:user:secret").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o644)).unwrap();

    cmd()
        .arg("--schema")
        .arg("public")
        .env("PGPASSFILE", f.path().to_str().unwrap())
        .env_remove("PG_PASSWORD")
        .assert()
        .stderr(predicate::str::contains("WARNING"));
}

#[cfg(unix)]
#[test]
fn test_pgpass_correct_permissions_no_warning() {
    use std::os::unix::fs::PermissionsExt;

    let mut f = Builder::new().suffix(".pgpass").tempfile().unwrap();
    writeln!(f.as_file_mut(), "localhost:5432:mydb:user:secret").unwrap();
    std::fs::set_permissions(f.path(), std::fs::Permissions::from_mode(0o600)).unwrap();

    cmd()
        .arg("--schema")
        .arg("public")
        .env("PGPASSFILE", f.path().to_str().unwrap())
        .env_remove("PG_PASSWORD")
        .assert()
        .stderr(predicate::str::contains("WARNING").not());
}

// ── Multi-flag combinations ────────────────────────────────────────────────────

#[test]
fn test_discover_all_schemas_with_dry_run() {
    cmd()
        .arg("--discover-all-schemas")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_all_valid_flags_together() {
    cmd()
        .arg("--schema")
        .arg("public,analytics")
        .arg("--dry-run")
        .arg("--mode")
        .arg("vacuum,analyze,freeze")
        .arg("--wraparound-min-age")
        .arg("150000000")
        .arg("--maintenance-work-mem-gb")
        .arg("2")
        .arg("--log-format")
        .arg("json")
        .arg("--sslmode")
        .arg("disable")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_table_with_dry_run() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--table")
        .arg("users")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── --wraparound-pct ───────────────────────────────────────────────────────────

#[test]
fn test_wraparound_pct_valid() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-pct")
        .arg("75")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_wraparound_pct_zero() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-pct")
        .arg("0")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_wraparound_pct_hundred() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-pct")
        .arg("100")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_wraparound_pct_above_hundred_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-pct")
        .arg("101")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("wraparound-pct"));
}

#[test]
fn test_wraparound_pct_negative_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-pct")
        .arg("-1")
        .env_clear()
        .assert()
        .failure();
}

#[test]
fn test_wraparound_pct_not_a_number_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-pct")
        .arg("high")
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_wraparound_pct_overrides_min_age() {
    // Both flags together — pct takes precedence; no validation error
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--wraparound-pct")
        .arg("80")
        .arg("--wraparound-min-age")
        .arg("150000000")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_help_contains_wraparound_pct() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--wraparound-pct"));
}

// ── --force flag ───────────────────────────────────────────────────────────────

#[test]
fn test_force_flag_parsing() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--force")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_force_with_dry_run() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--force")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_force_with_table_filter() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--force")
        .arg("--table")
        .arg("orders")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_help_contains_force() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"));
}

// ── --bloat-threshold-pct ─────────────────────────────────────────────────────

#[test]
fn test_bloat_threshold_pct_default() {
    cmd()
        .arg("--schema")
        .arg("public")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_bloat_threshold_pct_custom() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--bloat-threshold-pct")
        .arg("60.5")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_bloat_threshold_pct_zero() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--bloat-threshold-pct")
        .arg("0")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_bloat_threshold_pct_hundred() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--bloat-threshold-pct")
        .arg("100")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_bloat_threshold_pct_above_hundred_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--bloat-threshold-pct")
        .arg("101")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("bloat-threshold-pct"));
}

#[test]
fn test_bloat_threshold_pct_negative_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--bloat-threshold-pct")
        .arg("-1")
        .env_clear()
        .assert()
        .failure();
}

// ── Size filtering (min/max table size) ──────────────────────────────────────

#[test]
fn test_min_table_size_gb() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--min-table-size-gb")
        .arg("0.5")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_max_table_size_gb() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--max-table-size-gb")
        .arg("10")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_min_and_max_table_size_gb() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--min-table-size-gb")
        .arg("0.5")
        .arg("--max-table-size-gb")
        .arg("10")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_min_greater_than_max_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--min-table-size-gb")
        .arg("10")
        .arg("--max-table-size-gb")
        .arg("5")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("must be <="));
}

#[test]
fn test_negative_min_table_size_rejected() {
    // Negative numbers are rejected by clap as they look like flags
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--min-table-size-gb")
        .arg("-1")
        .env_clear()
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_negative_max_table_size_rejected() {
    // Negative numbers are rejected by clap as they look like flags
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--max-table-size-gb")
        .arg("-1")
        .env_clear()
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_fractional_gb_values() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--min-table-size-gb")
        .arg("0.001")
        .arg("--max-table-size-gb")
        .arg("1.5")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── I/O throttling (--gentle / --vacuum-cost-delay-ms / --vacuum-cost-limit) ────

#[test]
fn test_gentle_flag_accepted() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--gentle")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_vacuum_cost_flags_accepted() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--vacuum-cost-delay-ms")
        .arg("20")
        .arg("--vacuum-cost-limit")
        .arg("400")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_gentle_with_explicit_overrides_accepted() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--gentle")
        .arg("--vacuum-cost-limit")
        .arg("400")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_vacuum_cost_delay_zero_accepted() {
    // 0 is an explicit "disable throttling", not an invalid value
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--vacuum-cost-delay-ms")
        .arg("0")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_vacuum_cost_delay_at_max_boundary() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--vacuum-cost-delay-ms")
        .arg("100")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_vacuum_cost_delay_above_max_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--vacuum-cost-delay-ms")
        .arg("101")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("--vacuum-cost-delay-ms"))
        .stderr(predicate::str::contains("must be between 0 and 100"));
}

#[test]
fn test_vacuum_cost_delay_not_a_number_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--vacuum-cost-delay-ms")
        .arg("slow")
        .env_clear()
        .assert()
        .failure()
        .code(2);
}

#[test]
fn test_vacuum_cost_limit_boundaries_accepted() {
    for limit in ["1", "10000"] {
        cmd()
            .arg("--schema")
            .arg("public")
            .arg("--vacuum-cost-limit")
            .arg(limit)
            .env_clear()
            .assert()
            .code(predicate::ne(2));
    }
}

#[test]
fn test_vacuum_cost_limit_zero_rejected() {
    // PostgreSQL's vacuum_cost_limit floor is 1
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--vacuum-cost-limit")
        .arg("0")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("--vacuum-cost-limit"))
        .stderr(predicate::str::contains("must be between 1 and 10000"));
}

#[test]
fn test_vacuum_cost_limit_above_max_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--vacuum-cost-limit")
        .arg("10001")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("--vacuum-cost-limit"));
}

#[test]
fn test_help_contains_throttling_flags() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--gentle"))
        .stdout(predicate::str::contains("--vacuum-cost-delay-ms"))
        .stdout(predicate::str::contains("--vacuum-cost-limit"));
}

#[test]
fn test_config_file_with_throttling_keys() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(
        f,
        r#"
schema = "public"
gentle = true
vacuum-cost-delay-ms = 15.0
vacuum-cost-limit = 300
"#
    )
    .unwrap();

    cmd()
        .arg("--config")
        .arg(f.path())
        .env_clear()
        .assert()
        // Parses and validates; only the DB connection fails
        .code(predicate::ne(2))
        .stderr(predicate::str::contains("vacuum-cost").not());
}

#[test]
fn test_config_file_with_invalid_throttling_value_rejected() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(
        f,
        r#"
schema = "public"
vacuum-cost-limit = 50000
"#
    )
    .unwrap();

    cmd()
        .arg("--config")
        .arg(f.path())
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("--vacuum-cost-limit"));
}

// ── Connection string (--dsn / PG_DSN) ─────────────────────────────────────────

#[test]
fn test_dsn_uri_accepted() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice@db.host:5433/mydb")
        .env_clear()
        .assert()
        // Parses and validates; only the DB connection fails
        .code(predicate::ne(2));
}

#[test]
fn test_dsn_keyword_string_accepted() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("host=db.host port=5433 dbname=mydb user=alice")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_dsn_env_var_accepted() {
    cmd()
        .arg("--schema")
        .arg("public")
        .env_clear()
        .env("PG_DSN", "postgres://alice@db.host/mydb")
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_dsn_invalid_fails_before_connecting() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("not-a-dsn")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("postgres:// URI"));
}

#[test]
fn test_dsn_unsupported_sslmode_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://h/db?sslmode=prefer")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("sslmode"))
        .stderr(predicate::str::contains("verify-full"));
}

#[test]
fn test_dsn_multi_host_rejected() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://h1:5432,h2:5433/db")
        .env_clear()
        .assert()
        .failure()
        .stderr(predicate::str::contains("single host"));
}

#[test]
fn test_dsn_password_emits_insecurity_warning() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice:s3cr3t@db.host/mydb")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains(
            "connection string contains a password",
        ));
}

#[test]
fn test_dsn_without_password_no_warning() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice@db.host/mydb")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("contains a password").not());
}

#[test]
fn test_dsn_password_never_appears_in_output() {
    // The raw DSN must never reach stdout or the log; only a redacted form may.
    let out = cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice:hunter2@db.host/mydb")
        .env_clear()
        .output()
        .unwrap();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !combined.contains("hunter2"),
        "password leaked into output:\n{combined}"
    );
}

#[test]
fn test_explicit_flags_override_dsn_components() {
    // --database must win over the DSN's dbname. The startup log names the database
    // actually used, which is how the precedence is observable without a server.
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice@127.0.0.1:1/from_dsn")
        .arg("--database")
        .arg("from_flag")
        .arg("--log-file")
        .arg("/dev/null")
        .env_clear()
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "Connecting to database 'from_flag'",
        ));
}

#[test]
fn test_dsn_fills_fields_not_given_as_flags() {
    // The complement of the test above: with no --database, the DSN's dbname is used.
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice@127.0.0.1:1/from_dsn")
        .arg("--log-file")
        .arg("/dev/null")
        .env_clear()
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "Connecting to database 'from_dsn'",
        ));
}

#[test]
fn test_dsn_outranks_env_vars() {
    // Documented precedence: DSN components beat PG_* env vars.
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice@127.0.0.1:1/from_dsn")
        .arg("--log-file")
        .arg("/dev/null")
        .env_clear()
        .env("PG_DATABASE", "from_env")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "Connecting to database 'from_dsn'",
        ));
}

#[test]
fn test_env_var_fills_what_dsn_omits() {
    // A DSN with no dbname leaves the field open for PG_DATABASE.
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice@127.0.0.1:1")
        .arg("--log-file")
        .arg("/dev/null")
        .env_clear()
        .env("PG_DATABASE", "from_env")
        .assert()
        .failure()
        .stdout(predicate::str::contains(
            "Connecting to database 'from_env'",
        ));
}

#[test]
fn test_dsn_ignored_params_warned_about() {
    cmd()
        .arg("--schema")
        .arg("public")
        .arg("--dsn")
        .arg("postgres://alice@127.0.0.1:1/db?application_name=zzz")
        .arg("--log-file")
        .arg("/dev/null")
        .env_clear()
        .assert()
        .failure()
        .stdout(predicate::str::contains("application_name"))
        .stdout(predicate::str::contains("Ignoring connection string"));
}

#[test]
fn test_help_contains_dsn() {
    cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--dsn"));
}

#[test]
fn test_config_file_with_dsn_key() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(
        f,
        r#"
schema = "public"
dsn = "postgres://alice@db.host:5433/mydb"
"#
    )
    .unwrap();

    cmd()
        .arg("--config")
        .arg(f.path())
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_config_file_dsn_env_interpolation() {
    let mut f = Builder::new().suffix(".toml").tempfile().unwrap();
    writeln!(
        f,
        r#"
schema = "public"
dsn = "${{MY_TEST_DSN}}"
"#
    )
    .unwrap();

    cmd()
        .arg("--config")
        .arg(f.path())
        .env_clear()
        .env("MY_TEST_DSN", "postgres://alice@db.host/mydb")
        .assert()
        .code(predicate::ne(2));
}
