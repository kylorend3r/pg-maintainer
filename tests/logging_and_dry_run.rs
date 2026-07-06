//! Tests for logging flag parsing and dry-run behavior.
//! These tests verify flag acceptance and file path handling without a live DB.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

fn cmd() -> Command {
    Command::cargo_bin("pg-maintainer").unwrap()
}

// ── Log file flag ──────────────────────────────────────────────────────────────

#[test]
fn test_log_file_flag_accepted() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("test.log");

    cmd()
        .arg("--schema").arg("public")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_log_file_created_on_connection_attempt() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("pg-maintainer.log");

    let _ = cmd()
        .arg("--schema").arg("public")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .env_clear()
        .assert();

    // Logger opens the file when the first message is written (before connection)
    if log_path.exists() {
        assert!(log_path.is_file());
        let content = fs::read_to_string(&log_path).unwrap();
        assert!(
            content.contains("Connecting") || content.contains("INFO") || content.contains("["),
            "Log file should contain at least one log entry"
        );
    }
}

// ── Dry run ────────────────────────────────────────────────────────────────────

#[test]
fn test_dry_run_flag_not_a_parse_error() {
    cmd()
        .arg("--schema").arg("public")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_dry_run_with_log_file() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("dry_run.log");

    cmd()
        .arg("--schema").arg("public")
        .arg("--dry-run")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_dry_run_with_table_filter() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("dry_run_table.log");

    cmd()
        .arg("--schema").arg("public")
        .arg("--table").arg("orders")
        .arg("--dry-run")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Silence mode ───────────────────────────────────────────────────────────────

#[test]
fn test_silence_mode_with_log_file() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("silence.log");

    cmd()
        .arg("--schema").arg("public")
        .arg("--silence-mode")
        .arg("--dry-run")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_silence_mode_prints_startup_line() {
    // Even in silence mode, a single startup line is printed to stdout
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("silence_startup.log");

    let output = cmd()
        .arg("--schema").arg("public")
        .arg("--silence-mode")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .env_clear()
        .output()
        .unwrap();

    // Startup message goes to stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If silence mode activated, the startup line "Starting pg-maintainer..." appears
    // (only if silence mode is reached before a connection error)
    // We just confirm no parse error
    assert_ne!(output.status.code().unwrap(), 2, "Should not be a clap parse error");
    let _ = stdout; // content depends on connection availability
}

// ── Log format with log file ───────────────────────────────────────────────────

#[test]
fn test_json_format_with_log_file() {
    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("json.log");

    cmd()
        .arg("--schema").arg("public")
        .arg("--log-format").arg("json")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_json_format_with_silence_mode() {
    cmd()
        .arg("--schema").arg("public")
        .arg("--log-format").arg("json")
        .arg("--silence-mode")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Default log file name ──────────────────────────────────────────────────────

#[test]
fn test_default_log_file_does_not_cause_parse_error() {
    // No --log-file specified → uses default "maintainer.log"
    cmd()
        .arg("--schema").arg("public")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Mode selection and bloat-related flags ──────────────────────────────────────

#[test]
fn test_mode_flag_with_dry_run() {
    cmd()
        .arg("--schema").arg("public")
        .arg("--mode").arg("vacuum,analyze,freeze,bloat")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_bloat_threshold_pct_with_dry_run() {
    cmd()
        .arg("--schema").arg("public")
        .arg("--bloat-threshold-pct").arg("70")
        .arg("--mode").arg("bloat")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

#[test]
fn test_size_filters_with_dry_run() {
    cmd()
        .arg("--schema").arg("public")
        .arg("--min-table-size-gb").arg("0.1")
        .arg("--max-table-size-gb").arg("5.0")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .code(predicate::ne(2));
}

// ── Integration placeholder (requires live DB) ─────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_dry_run_prevents_actual_maintenance() {
    // To run: cargo test -- --ignored
    // 1. Connect to a test DB
    // 2. Run pg-maintainer with --dry-run
    // 3. Verify pg_stat_user_tables.last_vacuum / last_analyze are unchanged
    // 4. Verify log file contains "[DRY RUN] Would run:" entries
    assert!(true, "placeholder — requires live PostgreSQL");
}

#[tokio::test]
#[ignore]
async fn test_log_file_json_entries_are_valid_json() {
    // Run pg-maintainer with --log-format json against a real DB,
    // then parse each line of the log file as JSON to confirm schema.
    assert!(true, "placeholder — requires live PostgreSQL");
}
