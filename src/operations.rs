use crate::logging::{LogContext, LogLevel, Logger};
use crate::queries;
use crate::types::{
    BloatTableInfo, ExplicitTable, FreezeTableInfo, LagGateVerdict, LagObservation,
    OperationSummary, ReplicaLagGate, RunPolicy, StandbyLag, TableInfo, VacuumOptions,
};
use crate::vacuum_output;
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::watch;
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;

/// Result of a table maintenance operation.
struct OperationResult {
    dead_tuples_before: Option<i64>,
    dead_tuples_removed: Option<i64>,
}

/// Log entry details for a maintenance operation.
struct LogEntry<'a> {
    schema: &'a str,
    table: &'a str,
    operation: &'a str,
    mode: &'a str,
    status: &'a str,
    dead_tuples_before: Option<i64>,
    dead_tuples_removed: Option<i64>,
    duration_ms: i64,
    error_message: Option<&'a str>,
}

// ─── Concurrency guard ────────────────────────────────────────────────────────

/// Try to acquire a session-scoped advisory lock for the given schema list.
/// The lock ID is derived from a hash of the concatenated schema names.
/// Returns Ok(()) if the lock was acquired; Err if another pg-maintainer instance
/// is already running against these schemas (lock is already held elsewhere).
/// The lock is automatically released when the connection closes.
pub async fn try_acquire_schema_lock(client: &Client, schemas: &[String]) -> Result<()> {
    let schemas_str = schemas.join(",");
    // Use hashtext() to convert the schema list to a 32-bit hash suitable for advisory locks
    let row = client
        .query_one("SELECT hashtext($1)::int AS lock_id", &[&schemas_str])
        .await
        .map_err(|e| anyhow::anyhow!("Failed to compute schema lock ID: {e}"))?;

    let lock_id: i32 = row.get("lock_id");

    // pg_try_advisory_lock(int4) returns true if the lock was acquired, false if already held
    let row = client
        .query_one(
            "SELECT pg_try_advisory_lock($1::int) AS acquired",
            &[&lock_id],
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to acquire advisory lock: {e}"))?;

    let acquired: bool = row.get("acquired");
    if !acquired {
        return Err(anyhow::anyhow!(
            "Another pg-maintainer run is already active on schema(s): {schemas_str}. \
             Release the existing lock or wait for the other process to complete."
        ));
    }

    Ok(())
}

// ─── Logging to maintenance logbook ──────────────────────────────────────────────

/// Insert a maintenance operation log entry (only if not dry_run).
/// Logging failures are silently ignored to prevent operation failures.
async fn log_maintenance_operation(client: &Client, dry_run: bool, entry: LogEntry<'_>) {
    if dry_run {
        return; // Don't log during dry-run
    }

    let params: &[&(dyn tokio_postgres::types::ToSql + Sync)] = &[
        &entry.schema,
        &entry.table,
        &entry.operation,
        &entry.mode,
        &entry.status,
        &entry.dead_tuples_before,
        &entry.dead_tuples_removed,
        &entry.duration_ms,
        &entry.error_message,
    ];

    if client
        .execute(queries::INSERT_MAINTENANCE_LOG, params)
        .await
        .is_err()
    {
        // Silently ignore logging errors — they should not fail maintenance operations
    }
}

// ─── Discovery queries ────────────────────────────────────────────────────────

/// Returns tables in the given schemas that have never been vacuumed.
/// If `table` is Some, only that table is checked.
pub async fn find_never_vacuumed(
    client: &Client,
    schemas: &[String],
    table: Option<&str>,
    min_bytes: i64,
    max_bytes: i64,
    limit: i64,
) -> Result<Vec<TableInfo>> {
    // Vec<String> implements ToSql for array binding; &[String] does not.
    let schemas_vec: Vec<String> = schemas.to_vec();
    let rows = if let Some(tbl) = table {
        client
            .query(
                queries::FIND_NEVER_VACUUMED_TABLE,
                &[&schemas_vec, &tbl, &min_bytes, &max_bytes],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-vacuumed tables: {e}"))?
    } else {
        client
            .query(
                queries::FIND_NEVER_VACUUMED,
                &[&schemas_vec, &min_bytes, &max_bytes, &limit],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-vacuumed tables: {e}"))?
    };

    Ok(rows
        .into_iter()
        .map(|row| TableInfo {
            schema_name: row.get("schemaname"),
            table_name: row.get("tablename"),
            n_live_tup: row.get("n_live_tup"),
            n_dead_tup: row.get("n_dead_tup"),
        })
        .collect())
}

/// Returns tables in the given schemas that have never been analyzed.
/// If `table` is Some, only that table is checked.
pub async fn find_never_analyzed(
    client: &Client,
    schemas: &[String],
    table: Option<&str>,
    min_bytes: i64,
    max_bytes: i64,
    limit: i64,
) -> Result<Vec<TableInfo>> {
    let schemas_vec: Vec<String> = schemas.to_vec();
    let rows = if let Some(tbl) = table {
        client
            .query(
                queries::FIND_NEVER_ANALYZED_TABLE,
                &[&schemas_vec, &tbl, &min_bytes, &max_bytes],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-analyzed tables: {e}"))?
    } else {
        client
            .query(
                queries::FIND_NEVER_ANALYZED,
                &[&schemas_vec, &min_bytes, &max_bytes, &limit],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-analyzed tables: {e}"))?
    };

    Ok(rows
        .into_iter()
        .map(|row| TableInfo {
            schema_name: row.get("schemaname"),
            table_name: row.get("tablename"),
            n_live_tup: row.get("n_live_tup"),
            n_dead_tup: row.get("n_dead_tup"),
        })
        .collect())
}

/// Returns tables whose XID age exceeds `min_age`, ordered worst-first.
/// If `table` is Some, only that table is checked.
pub async fn find_wraparound_candidates(
    client: &Client,
    schemas: &[String],
    min_age: i64,
    table: Option<&str>,
    min_bytes: i64,
    max_bytes: i64,
    limit: i64,
) -> Result<Vec<FreezeTableInfo>> {
    let schemas_vec: Vec<String> = schemas.to_vec();
    let rows = if let Some(tbl) = table {
        client
            .query(
                queries::FIND_WRAPAROUND_CANDIDATES_TABLE,
                &[&schemas_vec, &min_age, &tbl, &min_bytes, &max_bytes],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query wraparound candidates: {e}"))?
    } else {
        client
            .query(
                queries::FIND_WRAPAROUND_CANDIDATES,
                &[&schemas_vec, &min_age, &min_bytes, &max_bytes, &limit],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query wraparound candidates: {e}"))?
    };

    Ok(rows
        .into_iter()
        .map(|row| FreezeTableInfo {
            schema_name: row.get("schema_name"),
            table_name: row.get("table_name"),
            xid_age: row.get("xid_age"),
            freeze_max_age: row.get("freeze_max_age"),
        })
        .collect())
}

/// Returns tables with excessive dead tuples (bloat candidates).
/// If `table` is Some, only that table is checked.
#[allow(clippy::too_many_arguments)]
pub async fn find_bloat_candidates(
    client: &Client,
    schemas: &[String],
    table: Option<&str>,
    bloat_threshold_pct: f64,
    bloat_min_dead_tup: i64,
    min_bytes: i64,
    max_bytes: i64,
    limit: i64,
) -> Result<Vec<BloatTableInfo>> {
    let schemas_vec: Vec<String> = schemas.to_vec();
    let rows = if let Some(tbl) = table {
        client
            .query(
                queries::FIND_BLOAT_CANDIDATES_TABLE,
                &[
                    &schemas_vec,
                    &tbl,
                    &bloat_threshold_pct,
                    &bloat_min_dead_tup,
                    &min_bytes,
                    &max_bytes,
                ],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query bloat candidates: {e}"))?
    } else {
        client
            .query(
                queries::FIND_BLOAT_CANDIDATES,
                &[
                    &schemas_vec,
                    &bloat_threshold_pct,
                    &bloat_min_dead_tup,
                    &min_bytes,
                    &max_bytes,
                    &limit,
                ],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query bloat candidates: {e}"))?
    };

    Ok(rows
        .into_iter()
        .map(|row| BloatTableInfo {
            schema_name: row.get("schemaname"),
            table_name: row.get("tablename"),
            n_live_tup: row.get("n_live_tup"),
            n_dead_tup: row.get("n_dead_tup"),
        })
        .collect())
}

// ─── Settings reads ───────────────────────────────────────────────────────────

/// Returns the server's autovacuum_freeze_max_age as a transaction count.
/// Used to convert a percentage threshold into an absolute XID age.
pub async fn get_freeze_max_age(client: &Client) -> Result<i64> {
    let row = client
        .query_one(queries::GET_FREEZE_MAX_AGE, &[])
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read autovacuum_freeze_max_age: {e}"))?;
    Ok(row.get::<_, i64>(0))
}

/// Returns the server's autovacuum_analyze_threshold and autovacuum_analyze_scale_factor
/// as configured on the connected server.
pub async fn get_analyze_settings(client: &Client) -> Result<(i64, f64)> {
    let row = client
        .query_one(queries::GET_ANALYZE_SETTINGS, &[])
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read autovacuum_analyze settings: {e}"))?;
    Ok((
        row.get::<_, i64>("analyze_threshold"),
        row.get::<_, f64>("analyze_scale_factor"),
    ))
}

/// Returns tables where modifications since the last analyze exceed the threshold.
/// If `table` is Some, only that table is checked.
#[allow(clippy::too_many_arguments)]
pub async fn find_stale_stats_candidates(
    client: &Client,
    schemas: &[String],
    table: Option<&str>,
    analyze_threshold: i64,
    analyze_scale_factor: f64,
    min_bytes: i64,
    max_bytes: i64,
    limit: i64,
) -> Result<Vec<crate::types::StaleStatsTableInfo>> {
    let schemas_vec: Vec<String> = schemas.to_vec();
    let rows = if let Some(tbl) = table {
        client
            .query(
                queries::FIND_STALE_STATS_TABLE,
                &[
                    &schemas_vec,
                    &tbl,
                    &analyze_threshold,
                    &analyze_scale_factor,
                    &min_bytes,
                    &max_bytes,
                ],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query stale-stats candidates: {e}"))?
    } else {
        client
            .query(
                queries::FIND_STALE_STATS,
                &[
                    &schemas_vec,
                    &analyze_threshold,
                    &analyze_scale_factor,
                    &min_bytes,
                    &max_bytes,
                    &limit,
                ],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query stale-stats candidates: {e}"))?
    };

    Ok(rows
        .into_iter()
        .map(|row| crate::types::StaleStatsTableInfo {
            schema_name: row.get("schemaname"),
            table_name: row.get("tablename"),
            n_live_tup: row.get("n_live_tup"),
            n_mod_since_analyze: row.get("n_mod_since_analyze"),
        })
        .collect())
}

/// Returns tables whose most recent VACUUM (manual or auto) is older than the
/// configured number of days. Never-vacuumed tables are excluded.
#[allow(clippy::too_many_arguments)]
pub async fn find_vacuum_overdue_candidates(
    client: &Client,
    schemas: &[String],
    table: Option<&str>,
    older_than_days: i32,
    min_bytes: i64,
    max_bytes: i64,
    limit: i64,
) -> Result<Vec<crate::types::OverdueVacuumTableInfo>> {
    let schemas_vec: Vec<String> = schemas.to_vec();
    let rows = if let Some(tbl) = table {
        client
            .query(
                queries::FIND_VACUUM_OVERDUE_TABLE,
                &[&schemas_vec, &tbl, &older_than_days, &min_bytes, &max_bytes],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query vacuum-overdue candidates: {e}"))?
    } else {
        client
            .query(
                queries::FIND_VACUUM_OVERDUE,
                &[
                    &schemas_vec,
                    &older_than_days,
                    &min_bytes,
                    &max_bytes,
                    &limit,
                ],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query vacuum-overdue candidates: {e}"))?
    };

    Ok(rows
        .into_iter()
        .map(|row| crate::types::OverdueVacuumTableInfo {
            schema_name: row.get("schemaname"),
            table_name: row.get("tablename"),
            n_live_tup: row.get("n_live_tup"),
            n_dead_tup: row.get("n_dead_tup"),
            days_since_vacuum: row.get("days_since_vacuum"),
        })
        .collect())
}

/// Returns tables whose most recent ANALYZE (manual or auto) is older than the
/// configured number of days. Never-analyzed tables are excluded.
#[allow(clippy::too_many_arguments)]
pub async fn find_analyze_overdue_candidates(
    client: &Client,
    schemas: &[String],
    table: Option<&str>,
    older_than_days: i32,
    min_bytes: i64,
    max_bytes: i64,
    limit: i64,
) -> Result<Vec<crate::types::OverdueAnalyzeTableInfo>> {
    let schemas_vec: Vec<String> = schemas.to_vec();
    let rows = if let Some(tbl) = table {
        client
            .query(
                queries::FIND_ANALYZE_OVERDUE_TABLE,
                &[&schemas_vec, &tbl, &older_than_days, &min_bytes, &max_bytes],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query analyze-overdue candidates: {e}"))?
    } else {
        client
            .query(
                queries::FIND_ANALYZE_OVERDUE,
                &[
                    &schemas_vec,
                    &older_than_days,
                    &min_bytes,
                    &max_bytes,
                    &limit,
                ],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query analyze-overdue candidates: {e}"))?
    };

    Ok(rows
        .into_iter()
        .map(|row| crate::types::OverdueAnalyzeTableInfo {
            schema_name: row.get("schemaname"),
            table_name: row.get("tablename"),
            n_live_tup: row.get("n_live_tup"),
            n_mod_since_analyze: row.get("n_mod_since_analyze"),
            days_since_analyze: row.get("days_since_analyze"),
        })
        .collect())
}

// ─── Active vacuum detection ──────────────────────────────────────────────────

/// Returns the PIDs of any VACUUM or autovacuum workers currently running on
/// the given table, along with backend_type so the caller can distinguish
/// autovacuum workers from manual VACUUM sessions.
async fn find_active_vacuums(
    client: &Client,
    schema: &str,
    table: &str,
) -> Result<Vec<(i32, String)>> {
    let rows = client
        .query(queries::FIND_ACTIVE_VACUUMS_ON_TABLE, &[&schema, &table])
        .await
        .map_err(|e| {
            anyhow::anyhow!("Failed to check active vacuums on \"{schema}\".\"{table}\" : {e}")
        })?;
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                row.get::<_, i32>("pid"),
                row.get::<_, String>("backend_type"),
            )
        })
        .collect())
}

/// Terminate the given backend PIDs via pg_terminate_backend().
async fn terminate_backends(client: &Client, pids: &[i32]) -> Result<()> {
    for pid in pids {
        client
            .execute("SELECT pg_terminate_backend($1)", &[pid])
            .await
            .map_err(|e| anyhow::anyhow!("pg_terminate_backend({pid}) failed: {e}"))?;
    }
    Ok(())
}

// ─── Lock timeout detection ───────────────────────────────────────────────────

/// Returns true when a table operation error was caused by lock_timeout
/// (SQLSTATE 55P03, "canceling statement due to lock timeout").
fn is_lock_timeout(err: &tokio_postgres::Error) -> bool {
    err.code() == Some(&SqlState::LOCK_NOT_AVAILABLE)
}

// ─── Individual table operations ──────────────────────────────────────────────

/// Double any embedded `"` so an identifier can't break out of its quoting.
fn quote_ident(s: &str) -> String {
    s.replace('"', "\"\"")
}

async fn vacuum_table(
    client: &Client,
    schema: &str,
    table: &str,
    vacuum_opts: VacuumOptions,
) -> Result<OperationResult, tokio_postgres::Error> {
    // Get dead tuple count before VACUUM
    let dead_before: i64 = client
        .query_one(queries::GET_DEAD_TUPLE_COUNT, &[&schema, &table])
        .await?
        .get(0);

    let mut opts = vec!["VERBOSE".to_string()];
    if !vacuum_opts.truncate {
        opts.push("TRUNCATE FALSE".to_string());
    }
    if vacuum_opts.disable_page_skipping {
        opts.push("DISABLE_PAGE_SKIPPING".to_string());
    }
    if vacuum_opts.skip_locked {
        opts.push("SKIP_LOCKED".to_string());
    }
    let sql = format!(
        "VACUUM ({}) \"{}\".\"{}\"",
        opts.join(", "),
        quote_ident(schema),
        quote_ident(table)
    );
    client.execute(&sql, &[]).await?;

    // Get dead tuple count after VACUUM
    let dead_after: i64 = client
        .query_one(queries::GET_DEAD_TUPLE_COUNT, &[&schema, &table])
        .await?
        .get(0);

    let removed = vacuum_output::get_dead_tuples_removed(dead_before, dead_after);
    Ok(OperationResult {
        dead_tuples_before: if dead_before > 0 {
            Some(dead_before)
        } else {
            None
        },
        dead_tuples_removed: removed,
    })
}

async fn analyze_table(
    client: &Client,
    schema: &str,
    table: &str,
) -> Result<OperationResult, tokio_postgres::Error> {
    let sql = format!(
        "ANALYZE \"{}\".\"{}\"",
        quote_ident(schema),
        quote_ident(table)
    );
    client.execute(&sql, &[]).await?;
    Ok(OperationResult {
        dead_tuples_before: None,
        dead_tuples_removed: None,
    })
}

async fn freeze_table(
    client: &Client,
    schema: &str,
    table: &str,
    vacuum_opts: VacuumOptions,
) -> Result<OperationResult, tokio_postgres::Error> {
    // INDEX_CLEANUP FALSE avoids index bloat during aggressive freeze passes.
    // VERBOSE surfaces progress notices to the PostgreSQL log.
    let mut opts = vec![
        "VERBOSE".to_string(),
        "FREEZE".to_string(),
        "INDEX_CLEANUP FALSE".to_string(),
    ];
    if !vacuum_opts.truncate {
        opts.push("TRUNCATE FALSE".to_string());
    }
    if vacuum_opts.disable_page_skipping {
        opts.push("DISABLE_PAGE_SKIPPING".to_string());
    }
    if vacuum_opts.skip_locked {
        opts.push("SKIP_LOCKED".to_string());
    }
    let sql = format!(
        "VACUUM ({}) \"{}\".\"{}\"",
        opts.join(", "),
        quote_ident(schema),
        quote_ident(table)
    );
    client.execute(&sql, &[]).await?;
    Ok(OperationResult {
        dead_tuples_before: None,
        dead_tuples_removed: None,
    })
}

// ─── Shared active-vacuum gate ────────────────────────────────────────────────

/// Check for active vacuums on `schema`.`table`.
///
/// Returns `true` if the caller should proceed with the operation, `false` if
/// the table should be skipped (recorded in `summary.skipped`).
///
/// Autovacuum workers are always terminated unconditionally (no --force needed).
/// Manual VACUUM sessions are only terminated if `force` is set; otherwise the
/// table is skipped.
///
/// In dry-run mode nothing is terminated; the function only logs what would happen
/// and still returns `true` when actions would proceed or `false` when the table
/// would be skipped.
async fn handle_active_vacuums(
    client: &Client,
    schema: &str,
    table: &str,
    policy: RunPolicy,
    logger: &Arc<Logger>,
    summary: &mut OperationSummary,
) -> Result<bool> {
    let active_sessions = find_active_vacuums(client, schema, table).await?;
    if active_sessions.is_empty() {
        return Ok(true); // no conflict — proceed
    }

    // If --skip-active-vacuum is set, always skip this table
    if policy.skip_active_vacuum {
        logger.log(
            LogLevel::Warning,
            &format!(
                "Skipping \"{}\".\"{}\" — {} active VACUUM session(s) found (--skip-active-vacuum)",
                schema,
                table,
                active_sessions.len()
            ),
        );
        summary.skipped += 1;
        return Ok(false);
    }

    let autovacuum_pids: Vec<i32> = active_sessions
        .iter()
        .filter(|(_, backend_type)| backend_type == BACKEND_TYPE_AUTOVACUUM_WORKER)
        .map(|(pid, _)| *pid)
        .collect();
    let manual_pids: Vec<i32> = active_sessions
        .iter()
        .filter(|(_, backend_type)| backend_type != BACKEND_TYPE_AUTOVACUUM_WORKER)
        .map(|(pid, _)| *pid)
        .collect();

    // Always terminate autovacuum workers
    if !autovacuum_pids.is_empty() {
        if policy.dry_run {
            logger.log(
                LogLevel::Warning,
                &format!(
                    "[DRY RUN] Would terminate {} autovacuum worker(s) on \"{}\".\"{}\" then proceed",
                    autovacuum_pids.len(),
                    schema,
                    table
                ),
            );
        } else {
            terminate_backends(client, &autovacuum_pids).await?;
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Terminated {} autovacuum worker(s) on \"{}\".\"{}\"",
                    autovacuum_pids.len(),
                    schema,
                    table
                ),
            );
        }
    }

    // Manual VACUUM sessions gate on --force
    if !manual_pids.is_empty() {
        if policy.force {
            if policy.dry_run {
                logger.log(
                    LogLevel::Warning,
                    &format!(
                        "[DRY RUN] Would terminate {} manual VACUUM session(s) on \"{}\".\"{}\" then proceed",
                        manual_pids.len(),
                        schema,
                        table
                    ),
                );
            } else {
                terminate_backends(client, &manual_pids).await?;
                logger.log(
                    LogLevel::Warning,
                    &format!(
                        "Terminated {} manual VACUUM session(s) on \"{}\".\"{}\" (--force)",
                        manual_pids.len(),
                        schema,
                        table
                    ),
                );
            }
            Ok(true) // proceed
        } else {
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Skipping \"{}\".\"{}\" — {} manual VACUUM session(s) running \
                     (use --force to terminate and proceed)",
                    schema,
                    table,
                    manual_pids.len()
                ),
            );
            summary.skipped += 1;
            Ok(false) // skip this table
        }
    } else {
        Ok(true) // autovacuum(s) terminated, no manual conflicts — proceed
    }
}

// ─── Operation runners ────────────────────────────────────────────────────────

const OP_VACUUM: &str = "VACUUM";
const OP_ANALYZE: &str = "ANALYZE";
const OP_FREEZE: &str = "VACUUM FREEZE";
const OP_BLOAT: &str = "VACUUM (BLOAT)";
const OP_VACUUM_OVERDUE: &str = "VACUUM (OVERDUE)";
const OP_ANALYZE_OVERDUE: &str = "ANALYZE (OVERDUE)";
const BACKEND_TYPE_AUTOVACUUM_WORKER: &str = "autovacuum worker";

/// Vacuum all tables that have never been vacuumed.
/// If `force` is true, active vacuums on the table are terminated before starting.
/// Otherwise tables with an active vacuum are skipped.
pub async fn run_vacuum_never_vacuumed(
    client: &Client,
    tables: &[TableInfo],
    policy: RunPolicy,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    vacuum_opts: VacuumOptions,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No never-vacuumed tables found.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} never-vacuumed table(s).", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
        // Check if shutdown was requested
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }
        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if policy.dry_run {
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: VACUUM \"{}\".\"{}\"  (live={}, dead={})",
                    t.schema_name, t.table_name, t.n_live_tup, t.n_dead_tup
                ),
            );
            continue;
        }

        logger.log_table_start(
            i + 1,
            tables.len(),
            &t.schema_name,
            &t.table_name,
            OP_VACUUM,
        );
        let start = Instant::now();
        match vacuum_table(client, &t.schema_name, &t.table_name, vacuum_opts).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(&t.schema_name, &t.table_name, OP_VACUUM, start.elapsed());
                if let Some(0) = result.dead_tuples_removed {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "VACUUM on \"{}\".\"{}\" removed 0 dead tuples — table may not have needed vacuuming, or another process already cleaned it up",
                            t.schema_name, t.table_name
                        ),
                    );
                } else if let Some(n) = result.dead_tuples_removed {
                    logger.log(
                        LogLevel::Info,
                        &format!(
                            "VACUUM on \"{}\".\"{}\" removed {n} dead tuple(s)",
                            t.schema_name, t.table_name
                        ),
                    );
                }
                log_maintenance_operation(
                    client,
                    policy.dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: "VACUUM",
                        mode: "never-vacuumed",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        OP_VACUUM,
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        policy.dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: "VACUUM",
                            mode: "never-vacuumed",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}

/// Analyze all tables that have never been analyzed.
/// If `table` is Some, only that table is checked and (if eligible) analyzed.
/// If `force` is true, active vacuums on the table are terminated before starting.
/// Otherwise tables with an active vacuum are skipped.
#[allow(clippy::too_many_arguments)]
pub async fn run_analyze_never_analyzed(
    client: &Client,
    tables: &[TableInfo],
    dry_run: bool,
    force: bool,
    skip_active_vacuum: bool,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No never-analyzed tables found.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} never-analyzed table(s).", tables.len()),
    );

    let policy = RunPolicy {
        dry_run,
        force,
        skip_active_vacuum,
    };

    for (i, t) in tables.iter().enumerate() {
        // Check if shutdown was requested
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }
        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if dry_run {
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: ANALYZE \"{}\".\"{}\"  (live={})",
                    t.schema_name, t.table_name, t.n_live_tup
                ),
            );
            continue;
        }

        logger.log_table_start(
            i + 1,
            tables.len(),
            &t.schema_name,
            &t.table_name,
            OP_ANALYZE,
        );
        let start = Instant::now();
        match analyze_table(client, &t.schema_name, &t.table_name).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(
                    &t.schema_name,
                    &t.table_name,
                    OP_ANALYZE,
                    start.elapsed(),
                );
                log_maintenance_operation(
                    client,
                    dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: "ANALYZE",
                        mode: "never-analyzed",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        OP_ANALYZE,
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: "ANALYZE",
                            mode: "never-analyzed",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}

/// Run VACUUM (VERBOSE, FREEZE, INDEX_CLEANUP FALSE) on all wraparound candidates.
/// If `force` is true, active vacuums on the table are terminated before starting.
/// Otherwise tables with an active vacuum are skipped.
pub async fn run_freeze_wraparound(
    client: &Client,
    tables: &[FreezeTableInfo],
    policy: RunPolicy,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    vacuum_opts: VacuumOptions,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(
            LogLevel::Success,
            "No wraparound candidates found — all tables are safely within the freeze window.",
        );
        return Ok(summary);
    }

    logger.log(
        LogLevel::Warning,
        &format!(
            "Found {} wraparound candidate(s) — these tables need immediate VACUUM FREEZE.",
            tables.len()
        ),
    );

    for t in tables {
        logger.log_with_context(
            LogLevel::Warning,
            &format!(
                "Wraparound candidate: \"{}\".\"{}\" — XID age {} ({:.1}% of freeze_max_age {})",
                t.schema_name,
                t.table_name,
                t.xid_age,
                t.pct_toward_wraparound(),
                t.freeze_max_age,
            ),
            LogContext {
                schema: Some(&t.schema_name),
                table_name: Some(&t.table_name),
                xid_age: Some(t.xid_age),
                ..Default::default()
            },
        );
    }

    for (i, t) in tables.iter().enumerate() {
        // Check if shutdown was requested
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }
        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if policy.dry_run {
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: VACUUM (VERBOSE, FREEZE, INDEX_CLEANUP FALSE) \"{}\".\"{}\"",
                    t.schema_name, t.table_name
                ),
            );
            continue;
        }

        logger.log_table_start(
            i + 1,
            tables.len(),
            &t.schema_name,
            &t.table_name,
            OP_FREEZE,
        );
        let start = Instant::now();
        match freeze_table(client, &t.schema_name, &t.table_name, vacuum_opts).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(&t.schema_name, &t.table_name, OP_FREEZE, start.elapsed());
                log_maintenance_operation(
                    client,
                    policy.dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: "FREEZE",
                        mode: "wraparound",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        OP_FREEZE,
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        policy.dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: "FREEZE",
                            mode: "wraparound",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}

/// Run VACUUM on all tables with excessive dead tuples (bloat).
/// If `table` is Some, only that table is checked and (if eligible) vacuumed.
/// If `force` is true, active vacuums on the table are terminated before starting.
/// Otherwise tables with an active vacuum are skipped.
/// Tables already vacuumed by earlier phases are skipped (tracked in `already_handled`).
#[allow(clippy::too_many_arguments)]
pub async fn run_bloat_vacuum(
    client: &Client,
    tables: &[BloatTableInfo],
    policy: RunPolicy,
    already_handled: &std::collections::HashSet<(String, String)>,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    vacuum_opts: VacuumOptions,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(
            LogLevel::Success,
            "No bloat candidates found — all tables within the threshold.",
        );
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} bloat candidate(s).", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
        // Check if shutdown was requested
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }

        if already_handled.contains(&(t.schema_name.clone(), t.table_name.clone())) {
            logger.log(
                LogLevel::Info,
                &format!(
                    "Skipping \"{}\".\"{}\" — already handled by an earlier phase",
                    t.schema_name, t.table_name
                ),
            );
            summary.skipped += 1;
            continue;
        }

        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if policy.dry_run {
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: VACUUM (VERBOSE) \"{}\".\"{}\"  (bloat={:.1}%)",
                    t.schema_name,
                    t.table_name,
                    t.pct_bloat()
                ),
            );
            continue;
        }

        logger.log_table_start(i + 1, tables.len(), &t.schema_name, &t.table_name, OP_BLOAT);
        let start = Instant::now();
        match vacuum_table(client, &t.schema_name, &t.table_name, vacuum_opts).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(&t.schema_name, &t.table_name, OP_BLOAT, start.elapsed());
                if let Some(0) = result.dead_tuples_removed {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "VACUUM on \"{}\".\"{}\" removed 0 dead tuples — table may not have needed vacuuming, or another process already cleaned it up",
                            t.schema_name, t.table_name
                        ),
                    );
                } else if let Some(n) = result.dead_tuples_removed {
                    logger.log(
                        LogLevel::Info,
                        &format!(
                            "VACUUM on \"{}\".\"{}\" removed {n} dead tuple(s)",
                            t.schema_name, t.table_name
                        ),
                    );
                }
                log_maintenance_operation(
                    client,
                    policy.dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: "VACUUM",
                        mode: "bloated",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        OP_BLOAT,
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        policy.dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: "VACUUM",
                            mode: "bloated",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}

/// Run ANALYZE on all tables with stale statistics.
/// If `table` is Some, only that table is checked and (if eligible) analyzed.
/// If `force` is true, active vacuums on the table are terminated before starting.
/// Otherwise tables with an active manual VACUUM are skipped (autovacuum is always terminated).
/// Tables already analyzed by earlier phases are skipped (tracked in `already_handled`).
#[allow(clippy::too_many_arguments)]
pub async fn run_stale_stats_analyze(
    client: &Client,
    tables: &[crate::types::StaleStatsTableInfo],
    analyze_threshold: i64,
    analyze_scale_factor: f64,
    dry_run: bool,
    force: bool,
    skip_active_vacuum: bool,
    already_handled: &std::collections::HashSet<(String, String)>,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No stale-stats candidates found.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} stale-stats candidate(s).", tables.len()),
    );

    let policy = RunPolicy {
        dry_run,
        force,
        skip_active_vacuum,
    };

    for (i, t) in tables.iter().enumerate() {
        // Check if shutdown was requested
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }

        if already_handled.contains(&(t.schema_name.clone(), t.table_name.clone())) {
            logger.log(
                LogLevel::Info,
                &format!(
                    "Skipping \"{}\".\"{}\" — already handled by an earlier phase",
                    t.schema_name, t.table_name
                ),
            );
            summary.skipped += 1;
            continue;
        }

        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if dry_run {
            let effective_threshold =
                t.effective_threshold(analyze_threshold, analyze_scale_factor);
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: ANALYZE \"{}\".\"{}\"  (mods={}, threshold={})",
                    t.schema_name, t.table_name, t.n_mod_since_analyze, effective_threshold
                ),
            );
            continue;
        }

        logger.log_table_start(
            i + 1,
            tables.len(),
            &t.schema_name,
            &t.table_name,
            "ANALYZE (STALE STATS)",
        );
        let start = Instant::now();
        match analyze_table(client, &t.schema_name, &t.table_name).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(
                    &t.schema_name,
                    &t.table_name,
                    "ANALYZE (STALE STATS)",
                    start.elapsed(),
                );
                log_maintenance_operation(
                    client,
                    dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: "ANALYZE",
                        mode: "stale-stats",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        "ANALYZE (STALE STATS)",
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: "ANALYZE",
                            mode: "stale-stats",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}

/// Run VACUUM on all tables overdue for vacuuming (not vacuumed in N days).
/// Tables already vacuumed by earlier phases are skipped (tracked in `already_vacuumed`).
#[allow(clippy::too_many_arguments)]
pub async fn run_vacuum_overdue(
    client: &Client,
    tables: &[crate::types::OverdueVacuumTableInfo],
    policy: RunPolicy,
    already_vacuumed: &std::collections::HashSet<(String, String)>,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    vacuum_opts: VacuumOptions,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No tables overdue for VACUUM.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} table(s) overdue for VACUUM.", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }

        if already_vacuumed.contains(&(t.schema_name.clone(), t.table_name.clone())) {
            logger.log(
                LogLevel::Info,
                &format!(
                    "Skipping \"{}\".\"{}\" — already vacuumed by an earlier phase",
                    t.schema_name, t.table_name
                ),
            );
            summary.skipped += 1;
            continue;
        }

        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if policy.dry_run {
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: VACUUM \"{}\".\"{}\"  (last vacuumed {:.1} days ago, live={}, dead={})",
                    t.schema_name, t.table_name, t.days_since_vacuum, t.n_live_tup, t.n_dead_tup
                ),
            );
            continue;
        }

        logger.log_table_start(
            i + 1,
            tables.len(),
            &t.schema_name,
            &t.table_name,
            OP_VACUUM_OVERDUE,
        );
        let start = Instant::now();
        match vacuum_table(client, &t.schema_name, &t.table_name, vacuum_opts).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(
                    &t.schema_name,
                    &t.table_name,
                    OP_VACUUM_OVERDUE,
                    start.elapsed(),
                );
                log_maintenance_operation(
                    client,
                    policy.dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: "VACUUM",
                        mode: "vacuum-overdue",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        OP_VACUUM_OVERDUE,
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        policy.dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: "VACUUM",
                            mode: "vacuum-overdue",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}

/// Run ANALYZE on all tables overdue for analysis (not analyzed in N days).
/// Tables already analyzed by earlier phases are skipped (tracked in `already_analyzed`).
#[allow(clippy::too_many_arguments)]
pub async fn run_analyze_overdue(
    client: &Client,
    tables: &[crate::types::OverdueAnalyzeTableInfo],
    policy: RunPolicy,
    already_analyzed: &std::collections::HashSet<(String, String)>,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No tables overdue for ANALYZE.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} table(s) overdue for ANALYZE.", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }

        if already_analyzed.contains(&(t.schema_name.clone(), t.table_name.clone())) {
            logger.log(
                LogLevel::Info,
                &format!(
                    "Skipping \"{}\".\"{}\" — already analyzed by an earlier phase",
                    t.schema_name, t.table_name
                ),
            );
            summary.skipped += 1;
            continue;
        }

        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if policy.dry_run {
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: ANALYZE \"{}\".\"{}\"  (last analyzed {:.1} days ago, mods={})",
                    t.schema_name, t.table_name, t.days_since_analyze, t.n_mod_since_analyze
                ),
            );
            continue;
        }

        logger.log_table_start(
            i + 1,
            tables.len(),
            &t.schema_name,
            &t.table_name,
            OP_ANALYZE_OVERDUE,
        );
        let start = Instant::now();
        match analyze_table(client, &t.schema_name, &t.table_name).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(
                    &t.schema_name,
                    &t.table_name,
                    OP_ANALYZE_OVERDUE,
                    start.elapsed(),
                );
                log_maintenance_operation(
                    client,
                    policy.dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: "ANALYZE",
                        mode: "analyze-overdue",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        OP_ANALYZE_OVERDUE,
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        policy.dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: "ANALYZE",
                            mode: "analyze-overdue",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}

// ─── Schema discovery ─────────────────────────────────────────────────────────

/// Discover all user-visible schemas (excludes system and temporary schemas).
pub async fn discover_all_user_schemas(client: &Client) -> Result<Vec<String>> {
    let rows = client
        .query(queries::GET_ALL_USER_SCHEMAS, &[])
        .await
        .map_err(|e| anyhow::anyhow!("Failed to discover schemas: {e}"))?;

    Ok(rows
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect())
}

// ─── Replication lag ──────────────────────────────────────────────────────────

/// Read every standby's replay lag from the primary.
///
/// Probes role membership first: without `pg_read_all_stats` the view returns no
/// rows, which would otherwise be indistinguishable from a healthy cluster with
/// no replicas.
pub async fn observe_replica_lag(client: &Client) -> Result<LagObservation> {
    let can_read: bool = client
        .query_one(queries::GET_CAN_READ_REPLICATION_STATS, &[])
        .await
        .map_err(|e| anyhow::anyhow!("Failed to check replication-stats privileges: {e}"))?
        .get("can_read");

    if !can_read {
        return Ok(LagObservation::Unobservable);
    }

    let rows = client
        .query(queries::GET_REPLICATION_LAG, &[])
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read pg_stat_replication: {e}"))?;

    if rows.is_empty() {
        return Ok(LagObservation::NoReplicas);
    }

    Ok(LagObservation::Observed(
        rows.into_iter()
            .map(|row| StandbyLag {
                application_name: row.get("application_name"),
                state: row.get("state"),
                sync_state: row.get("sync_state"),
                replay_lag_seconds: row.get("replay_lag_seconds"),
            })
            .collect(),
    ))
}

/// Describe a standby for a log line.
fn describe_standby(s: &StandbyLag) -> String {
    let name = if s.application_name.is_empty() {
        "<unnamed>"
    } else {
        &s.application_name
    };
    match s.replay_lag_seconds {
        Some(lag) => format!("{name} ({}, {}) {lag:.1}s behind", s.state, s.sync_state),
        None => format!("{name} ({}, {}) caught up", s.state, s.sync_state),
    }
}

/// Hold maintenance on one table until replication lag falls under the threshold.
///
/// Returns `Proceed` when the table may be maintained, `SkipTable` when the wait
/// expired, and `ShutdownRequested` when a signal arrived mid-wait.
pub async fn wait_for_replica_lag(
    client: &Client,
    gate: &ReplicaLagGate,
    schema: &str,
    table: &str,
    policy: RunPolicy,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<LagGateVerdict> {
    if gate.is_disabled() {
        return Ok(LagGateVerdict::Proceed);
    }

    let observation = observe_replica_lag(client).await?;

    match observation {
        LagObservation::Unobservable => {
            gate.disable();
            logger.log(
                LogLevel::Warning,
                "Cannot observe replication lag — the connected role lacks pg_read_all_stats \
                 (granted by pg_monitor). Lag gating is disabled for this run; grant the role \
                 pg_monitor to enable it.",
            );
            return Ok(LagGateVerdict::Proceed);
        }
        LagObservation::NoReplicas => {
            gate.disable();
            logger.log(
                LogLevel::Info,
                "No replicas are streaming from this server — replication-lag gating is \
                 disabled for this run.",
            );
            return Ok(LagGateVerdict::Proceed);
        }
        LagObservation::Observed(_) => {}
    }

    // Within threshold: proceed silently. Logging here would add a line per table.
    if !observation.exceeds(gate.threshold_seconds) {
        return Ok(LagGateVerdict::Proceed);
    }

    let observed = observation.max_lag_seconds().unwrap_or(0.0);

    if policy.dry_run {
        logger.log(
            LogLevel::Info,
            &format!(
                "[DRY RUN] Would wait before \"{schema}\".\"{table}\" — replication lag {observed:.1}s \
                 exceeds {:.1}s",
                gate.threshold_seconds
            ),
        );
        return Ok(LagGateVerdict::Proceed);
    }

    if gate.max_wait_seconds == 0 {
        gate.record_skip();
        logger.log(
            LogLevel::Warning,
            &format!(
                "Skipping \"{schema}\".\"{table}\" — replication lag {observed:.1}s exceeds \
                 {:.1}s and --max-replica-lag-wait-seconds is 0",
                gate.threshold_seconds
            ),
        );
        return Ok(LagGateVerdict::SkipTable);
    }

    logger.log(
        LogLevel::Info,
        &format!(
            "Replication lag {observed:.1}s exceeds {:.1}s — waiting up to {}s before \
             \"{schema}\".\"{table}\"",
            gate.threshold_seconds, gate.max_wait_seconds
        ),
    );

    let started = Instant::now();
    let deadline = std::time::Duration::from_secs(gate.max_wait_seconds);
    let poll = std::time::Duration::from_secs(gate.poll_interval_seconds.max(1));

    loop {
        // Never sleep past the deadline: with a 5s poll and a 12s budget, sleeping a
        // full interval each time would overshoot to 15s and make the "up to Ns"
        // message a lie.
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            let waited = started.elapsed();
            gate.add_waited(waited);
            gate.record_skip();
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Skipping \"{schema}\".\"{table}\" — replication lag did not recover \
                     within {:.0}s",
                    waited.as_secs_f64()
                ),
            );
            return Ok(LagGateVerdict::SkipTable);
        }

        // A signal must interrupt the sleep rather than be noticed after it.
        tokio::select! {
            _ = tokio::time::sleep(poll.min(remaining)) => {}
            _ = shutdown_rx.changed() => {
                gate.add_waited(started.elapsed());
                logger.log(
                    LogLevel::Warning,
                    "Shutdown signal received while waiting for replication lag.",
                );
                return Ok(LagGateVerdict::ShutdownRequested);
            }
        }

        let current = observe_replica_lag(client).await?;
        let current_lag = current.max_lag_seconds().unwrap_or(0.0);

        if !current.exceeds(gate.threshold_seconds) {
            let waited = started.elapsed();
            gate.add_waited(waited);
            logger.log(
                LogLevel::Info,
                &format!(
                    "Replication lag recovered to {current_lag:.1}s after {:.0}s — proceeding \
                     with \"{schema}\".\"{table}\"",
                    waited.as_secs_f64()
                ),
            );
            return Ok(LagGateVerdict::Proceed);
        }

        if started.elapsed() >= deadline {
            let waited = started.elapsed();
            gate.add_waited(waited);
            gate.record_skip();
            let worst = current
                .worst_standby()
                .map(describe_standby)
                .unwrap_or_else(|| "unknown standby".to_string());
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Skipping \"{schema}\".\"{table}\" — replication lag still {current_lag:.1}s \
                     after waiting {:.0}s (worst: {worst})",
                    waited.as_secs_f64()
                ),
            );
            return Ok(LagGateVerdict::SkipTable);
        }
    }
}

/// Log the cluster's replication state once, before any maintenance runs.
pub async fn log_initial_replica_lag(
    client: &Client,
    gate: &ReplicaLagGate,
    logger: &Arc<Logger>,
) -> Result<()> {
    match observe_replica_lag(client).await? {
        LagObservation::Unobservable => {
            gate.disable();
            logger.log(
                LogLevel::Warning,
                "Cannot observe replication lag — the connected role lacks pg_read_all_stats \
                 (granted by pg_monitor). Lag gating is disabled for this run.",
            );
        }
        LagObservation::NoReplicas => {
            gate.disable();
            logger.log(
                LogLevel::Info,
                "Replication-lag gating requested, but no replicas are streaming from this \
                 server — gating is disabled for this run.",
            );
        }
        observed @ LagObservation::Observed(_) => {
            let detail = match &observed {
                LagObservation::Observed(standbys) => standbys
                    .iter()
                    .map(describe_standby)
                    .collect::<Vec<_>>()
                    .join("; "),
                _ => String::new(),
            };
            logger.log(
                LogLevel::Info,
                &format!(
                    "Replication-lag gating active at {:.1}s (max wait {}s) — {} standby(s): {}",
                    gate.threshold_seconds,
                    gate.max_wait_seconds,
                    observed.standby_count(),
                    detail
                ),
            );
        }
    }
    Ok(())
}

// ─── Explicit table list (--also-tables) ──────────────────────────────────────

const OP_VACUUM_ANALYZE: &str = "VACUUM ANALYZE";

/// Narrow an explicit table list to the entries that can actually be maintained.
///
/// Two gates, each warning rather than failing so one bad entry cannot cost the
/// operator the whole list:
///   * the schema must be one of the resolved schemas, because the advisory lock
///     is derived from exactly that list — maintaining outside it would run
///     without the concurrency guard;
///   * the table must exist.
pub async fn resolve_explicit_tables(
    client: &Client,
    requested: &[ExplicitTable],
    schemas: &[String],
    logger: &Arc<Logger>,
) -> Result<Vec<ExplicitTable>> {
    if requested.is_empty() {
        return Ok(Vec::new());
    }

    let mut in_scope: Vec<ExplicitTable> = Vec::new();
    for t in requested {
        if t.schema_name == crate::config::LOGBOOK_SCHEMA_NAME {
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Ignoring \"{}\".\"{}\" — the {} schema is managed internally by pg-maintainer",
                    t.schema_name,
                    t.table_name,
                    crate::config::LOGBOOK_SCHEMA_NAME
                ),
            );
            continue;
        }
        if !schemas.contains(&t.schema_name) {
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Ignoring \"{}\".\"{}\" — schema \"{}\" is not in the schema list for this \
                     run; add it to --schema to include it",
                    t.schema_name, t.table_name, t.schema_name
                ),
            );
            continue;
        }
        in_scope.push(t.clone());
    }

    if in_scope.is_empty() {
        return Ok(Vec::new());
    }

    let schema_names: Vec<String> = in_scope.iter().map(|t| t.schema_name.clone()).collect();
    let table_names: Vec<String> = in_scope.iter().map(|t| t.table_name.clone()).collect();

    let rows = client
        .query(
            queries::FIND_EXPLICIT_TABLES,
            &[&schema_names, &table_names],
        )
        .await
        .map_err(|e| anyhow::anyhow!("Failed to resolve --also-tables entries: {e}"))?;

    let existing: std::collections::HashSet<(String, String)> = rows
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>("schema_name"),
                row.get::<_, String>("table_name"),
            )
        })
        .collect();

    // Preserve the order the operator wrote, so the log reads the way the flag did.
    let mut resolved = Vec::new();
    for t in in_scope {
        if existing.contains(&(t.schema_name.clone(), t.table_name.clone())) {
            resolved.push(t);
        } else {
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Ignoring \"{}\".\"{}\" — no such table, materialized view or partitioned \
                     table (names are matched exactly, and PostgreSQL folds unquoted identifiers \
                     to lower case)",
                    t.schema_name, t.table_name
                ),
            );
        }
    }

    Ok(resolved)
}

/// VACUUM (ANALYZE) one table, reusing vacuum_table's before/after dead-tuple
/// polling so the existing removal reporting works unchanged.
async fn vacuum_analyze_table(
    client: &Client,
    schema: &str,
    table: &str,
    vacuum_opts: VacuumOptions,
) -> Result<OperationResult, tokio_postgres::Error> {
    let dead_before: i64 = client
        .query_one(queries::GET_DEAD_TUPLE_COUNT, &[&schema, &table])
        .await?
        .get(0);

    let mut opts = vec!["VERBOSE".to_string(), "ANALYZE".to_string()];
    if !vacuum_opts.truncate {
        opts.push("TRUNCATE FALSE".to_string());
    }
    if vacuum_opts.disable_page_skipping {
        opts.push("DISABLE_PAGE_SKIPPING".to_string());
    }
    if vacuum_opts.skip_locked {
        opts.push("SKIP_LOCKED".to_string());
    }
    let sql = format!(
        "VACUUM ({}) \"{}\".\"{}\"",
        opts.join(", "),
        quote_ident(schema),
        quote_ident(table)
    );
    client.execute(&sql, &[]).await?;

    let dead_after: i64 = client
        .query_one(queries::GET_DEAD_TUPLE_COUNT, &[&schema, &table])
        .await?
        .get(0);

    let removed = vacuum_output::get_dead_tuples_removed(dead_before, dead_after);
    Ok(OperationResult {
        dead_tuples_before: if dead_before > 0 {
            Some(dead_before)
        } else {
            None
        },
        dead_tuples_removed: removed,
    })
}

/// Run VACUUM (ANALYZE) on every explicitly listed table, unconditionally.
///
/// No discovery criteria apply — that is the point of the flag. Tables already
/// maintained by an earlier phase are skipped, and every safety mechanic
/// (active-vacuum handling, lock_timeout, dry run, shutdown, lag gating) behaves
/// exactly as it does in the discovery phases.
#[allow(clippy::too_many_arguments)]
pub async fn run_also_tables(
    client: &Client,
    tables: &[ExplicitTable],
    policy: RunPolicy,
    already_handled: &std::collections::HashSet<(String, String)>,
    logger: &Arc<Logger>,
    shutdown_rx: &mut watch::Receiver<bool>,
    vacuum_opts: VacuumOptions,
    lag_gate: Option<&ReplicaLagGate>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary {
        total: tables.len(),
        ..Default::default()
    };

    if tables.is_empty() {
        logger.log(
            LogLevel::Success,
            "No explicitly listed tables to maintain.",
        );
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Maintaining {} explicitly listed table(s).", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
        if *shutdown_rx.borrow() {
            logger.log(
                LogLevel::Warning,
                "Shutdown signal received — stopping after current table.",
            );
            break;
        }

        if already_handled.contains(&(t.schema_name.clone(), t.table_name.clone())) {
            logger.log(
                LogLevel::Info,
                &format!(
                    "Skipping \"{}\".\"{}\" — already handled by an earlier phase",
                    t.schema_name, t.table_name
                ),
            );
            summary.skipped += 1;
            continue;
        }

        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        if let Some(gate) = lag_gate {
            match wait_for_replica_lag(
                client,
                gate,
                &t.schema_name,
                &t.table_name,
                policy,
                logger,
                shutdown_rx,
            )
            .await?
            {
                LagGateVerdict::Proceed => {}
                LagGateVerdict::SkipTable => {
                    summary.skipped += 1;
                    continue;
                }
                LagGateVerdict::ShutdownRequested => break,
            }
        }

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            policy,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if policy.dry_run {
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: VACUUM (VERBOSE, ANALYZE) \"{}\".\"{}\"",
                    t.schema_name, t.table_name
                ),
            );
            continue;
        }

        logger.log_table_start(
            i + 1,
            tables.len(),
            &t.schema_name,
            &t.table_name,
            OP_VACUUM_ANALYZE,
        );
        let start = Instant::now();
        match vacuum_analyze_table(client, &t.schema_name, &t.table_name, vacuum_opts).await {
            Ok(result) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                logger.log_table_success(
                    &t.schema_name,
                    &t.table_name,
                    OP_VACUUM_ANALYZE,
                    start.elapsed(),
                );
                if let Some(n) = result.dead_tuples_removed
                    && n > 0
                {
                    logger.log(
                        LogLevel::Info,
                        &format!(
                            "VACUUM (ANALYZE) on \"{}\".\"{}\" removed {n} dead tuple(s)",
                            t.schema_name, t.table_name
                        ),
                    );
                }
                log_maintenance_operation(
                    client,
                    policy.dry_run,
                    LogEntry {
                        schema: &t.schema_name,
                        table: &t.table_name,
                        operation: OP_VACUUM_ANALYZE,
                        mode: "also-tables",
                        status: "success",
                        dead_tuples_before: result.dead_tuples_before,
                        dead_tuples_removed: result.dead_tuples_removed,
                        duration_ms,
                        error_message: None,
                    },
                )
                .await;
                summary.succeeded += 1;
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis() as i64;
                if is_lock_timeout(&e) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(
                        &t.schema_name,
                        &t.table_name,
                        OP_VACUUM_ANALYZE,
                        &e.to_string(),
                    );
                    log_maintenance_operation(
                        client,
                        policy.dry_run,
                        LogEntry {
                            schema: &t.schema_name,
                            table: &t.table_name,
                            operation: OP_VACUUM_ANALYZE,
                            mode: "also-tables",
                            status: "error",
                            dead_tuples_before: None,
                            dead_tuples_removed: None,
                            duration_ms,
                            error_message: Some(&e.to_string()),
                        },
                    )
                    .await;
                    summary.failed += 1;
                }
            }
        }
    }

    Ok(summary)
}
