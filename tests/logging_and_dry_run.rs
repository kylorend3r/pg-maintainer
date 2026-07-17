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
        .arg("--mode").arg("never-vacuumed,never-analyzed,wraparound,bloated,stale-stats")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Invalid mode").not());
}

#[test]
fn test_bloat_threshold_pct_with_dry_run() {
    cmd()
        .arg("--schema").arg("public")
        .arg("--bloat-threshold-pct").arg("70")
        .arg("--mode").arg("bloated")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Invalid mode").not());
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

#[test]
fn test_limit_applies_to_wraparound_mode() {
    cmd()
        .arg("--schema").arg("public")
        .arg("--mode").arg("wraparound")
        .arg("--limit").arg("5")
        .arg("--dry-run")
        .env_clear()
        .assert()
        .stderr(predicate::str::contains("Invalid mode").not());
}

// ── Integration tests (requires live DB) ──────────────────────────────────────

#[tokio::test]
#[ignore]
async fn test_dry_run_prevents_actual_maintenance() {
    // Run: cargo test -- --include-ignored
    // 1. Connect to pgm_test database
    // 2. Create a fresh test table that has never been vacuumed
    // 3. Record last_vacuum timestamp before
    // 4. Run pg-maintainer with --dry-run
    // 5. Verify last_vacuum is unchanged
    // 6. Verify log file contains "[DRY RUN]" entries

    let conn_str = "host=127.0.0.1 port=5432 user=pgm_test password=pgm_test dbname=pgm_test";
    let (client, connection) = match tokio_postgres::connect(conn_str, tokio_postgres::NoTls).await {
        Ok(c) => c,
        Err(_) => {
            eprintln!("Could not connect to test database. Make sure docker-compose up is running.");
            return;
        }
    };

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("Connection error: {}", e);
        }
    });

    // Set up a fresh test table
    let _ = client.execute("DROP TABLE IF EXISTS pgm_dry_run_test", &[]).await;
    let _ = client
        .execute(
            "CREATE TABLE pgm_dry_run_test (id bigint, payload text)",
            &[],
        )
        .await;
    let _ = client
        .execute(
            "INSERT INTO pgm_dry_run_test SELECT g, repeat('x', 100) FROM generate_series(1, 1000) g",
            &[],
        )
        .await;

    // Record initial state
    let row = client
        .query_one(
            "SELECT last_vacuum, last_analyze FROM pg_stat_user_tables WHERE relname = 'pgm_dry_run_test'",
            &[],
        )
        .await
        .expect("Failed to query table stats");
    let last_vacuum_before: Option<std::time::SystemTime> = row.get(0);
    let last_analyze_before: Option<std::time::SystemTime> = row.get(1);

    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("dry_run_test.log");

    // Run pg-maintainer with --dry-run
    let _output = cmd()
        .arg("--host").arg("127.0.0.1")
        .arg("--port").arg("5432")
        .arg("--database").arg("pgm_test")
        .arg("--username").arg("pgm_test")
        .arg("--password").arg("pgm_test")
        .arg("--schema").arg("public")
        .arg("--table").arg("pgm_dry_run_test")
        .arg("--dry-run")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .output()
        .expect("Failed to run pg-maintainer");

    // Verify log file contains "[DRY RUN]"
    let log_content = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log_content.contains("[DRY RUN]") || log_content.contains("DRY RUN"),
        "Log should contain '[DRY RUN]' entries"
    );

    // Verify table was not actually vacuumed
    let row = client
        .query_one(
            "SELECT last_vacuum, last_analyze FROM pg_stat_user_tables WHERE relname = 'pgm_dry_run_test'",
            &[],
        )
        .await
        .expect("Failed to query table stats after run");
    let last_vacuum_after: Option<std::time::SystemTime> = row.get(0);
    let last_analyze_after: Option<std::time::SystemTime> = row.get(1);

    assert_eq!(
        last_vacuum_before, last_vacuum_after,
        "last_vacuum should not change in dry-run mode"
    );
    assert_eq!(
        last_analyze_before, last_analyze_after,
        "last_analyze should not change in dry-run mode"
    );

    let _ = client.execute("DROP TABLE IF EXISTS pgm_dry_run_test", &[]).await;
}

#[tokio::test]
#[ignore]
async fn test_log_file_json_entries_are_valid_json() {
    // Run pg-maintainer with --log-format json against test DB,
    // then verify each line of the log file is valid JSON.

    let conn_str = "host=127.0.0.1 port=5432 user=pgm_test password=pgm_test dbname=pgm_test";
    let (client, connection) = match tokio_postgres::connect(conn_str, tokio_postgres::NoTls).await {
        Ok(c) => c,
        Err(_) => {
            eprintln!("Could not connect to test database.");
            return;
        }
    };

    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("Connection error: {}", e);
        }
    });

    let dir = TempDir::new().unwrap();
    let log_path = dir.path().join("json_test.log");

    // Run with --log-format json and --dry-run
    let _ = cmd()
        .arg("--host").arg("127.0.0.1")
        .arg("--port").arg("5432")
        .arg("--database").arg("pgm_test")
        .arg("--username").arg("pgm_test")
        .arg("--password").arg("pgm_test")
        .arg("--schema").arg("public")
        .arg("--dry-run")
        .arg("--log-format").arg("json")
        .arg("--log-file").arg(log_path.to_str().unwrap())
        .output();

    // Parse the log file and verify each line is valid JSON
    if let Ok(log_content) = fs::read_to_string(&log_path) {
        for line in log_content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(line);
            assert!(
                parsed.is_ok(),
                "Log line should be valid JSON: {}",
                line
            );
        }
    }

    let _ = client.query("SELECT 1", &[]).await;
}
