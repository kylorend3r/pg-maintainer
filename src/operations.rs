use crate::logging::{LogContext, LogLevel, Logger};
use crate::queries;
use crate::types::{BloatTableInfo, FreezeTableInfo, OperationSummary, TableInfo};
use anyhow::Result;
use std::sync::Arc;
use std::time::Instant;
use tokio_postgres::Client;

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
            .query(queries::FIND_NEVER_VACUUMED_TABLE, &[&schemas_vec, &tbl, &min_bytes, &max_bytes])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-vacuumed tables: {}", e))?
    } else {
        client
            .query(queries::FIND_NEVER_VACUUMED, &[&schemas_vec, &min_bytes, &max_bytes, &limit])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-vacuumed tables: {}", e))?
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
            .query(queries::FIND_NEVER_ANALYZED_TABLE, &[&schemas_vec, &tbl, &min_bytes, &max_bytes])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-analyzed tables: {}", e))?
    } else {
        client
            .query(queries::FIND_NEVER_ANALYZED, &[&schemas_vec, &min_bytes, &max_bytes, &limit])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query never-analyzed tables: {}", e))?
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
            .map_err(|e| anyhow::anyhow!("Failed to query wraparound candidates: {}", e))?
    } else {
        client
            .query(queries::FIND_WRAPAROUND_CANDIDATES, &[&schemas_vec, &min_age, &min_bytes, &max_bytes, &limit])
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query wraparound candidates: {}", e))?
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
                &[&schemas_vec, &tbl, &bloat_threshold_pct, &bloat_min_dead_tup, &min_bytes, &max_bytes],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query bloat candidates: {}", e))?
    } else {
        client
            .query(
                queries::FIND_BLOAT_CANDIDATES,
                &[&schemas_vec, &bloat_threshold_pct, &bloat_min_dead_tup, &min_bytes, &max_bytes, &limit],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query bloat candidates: {}", e))?
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
        .map_err(|e| anyhow::anyhow!("Failed to read autovacuum_freeze_max_age: {}", e))?;
    Ok(row.get::<_, i64>(0))
}

/// Returns the server's autovacuum_analyze_threshold and autovacuum_analyze_scale_factor
/// as configured on the connected server.
pub async fn get_analyze_settings(client: &Client) -> Result<(i64, f64)> {
    let row = client
        .query_one(queries::GET_ANALYZE_SETTINGS, &[])
        .await
        .map_err(|e| anyhow::anyhow!("Failed to read autovacuum_analyze settings: {}", e))?;
    Ok((row.get::<_, i64>("analyze_threshold"), row.get::<_, f64>("analyze_scale_factor")))
}

/// Returns tables where modifications since the last analyze exceed the threshold.
/// If `table` is Some, only that table is checked.
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
                &[&schemas_vec, &tbl, &analyze_threshold, &analyze_scale_factor, &min_bytes, &max_bytes],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query stale-stats candidates: {}", e))?
    } else {
        client
            .query(
                queries::FIND_STALE_STATS,
                &[&schemas_vec, &analyze_threshold, &analyze_scale_factor, &min_bytes, &max_bytes, &limit],
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to query stale-stats candidates: {}", e))?
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

// ─── Active vacuum detection ──────────────────────────────────────────────────

/// Returns the PIDs of any VACUUM or autovacuum workers currently running on
/// the given table, along with backend_type so the caller can distinguish
/// autovacuum workers from manual VACUUM sessions.
async fn find_active_vacuums(client: &Client, schema: &str, table: &str) -> Result<Vec<(i32, String)>> {
    let rows = client
        .query(queries::FIND_ACTIVE_VACUUMS_ON_TABLE, &[&schema, &table])
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to check active vacuums on \"{}\".\"{}\" : {}",
                schema,
                table,
                e
            )
        })?;
    Ok(rows.into_iter().map(|row| (row.get::<_, i32>("pid"), row.get::<_, String>("backend_type"))).collect())
}

/// Terminate the given backend PIDs via pg_terminate_backend().
async fn terminate_backends(client: &Client, pids: &[i32]) -> Result<()> {
    for pid in pids {
        client
            .execute("SELECT pg_terminate_backend($1)", &[pid])
            .await
            .map_err(|e| anyhow::anyhow!("pg_terminate_backend({}) failed: {}", pid, e))?;
    }
    Ok(())
}

// ─── Lock timeout detection ───────────────────────────────────────────────────

/// Returns true when a table operation error was caused by lock_timeout.
/// PostgreSQL emits "canceling statement due to lock timeout" (SQLSTATE 55P03).
fn is_lock_timeout(err: &str) -> bool {
    err.contains("lock timeout")
}

// ─── Individual table operations ──────────────────────────────────────────────

async fn vacuum_table(client: &Client, schema: &str, table: &str) -> Result<()> {
    // Table names originate from pg_catalog — quoting them is sufficient protection.
    let sql = format!("VACUUM (VERBOSE) \"{}\".\"{}\"", schema, table);
    client
        .execute(&sql, &[])
        .await
        .map_err(|e| anyhow::anyhow!("VACUUM failed: {}", e))?;
    Ok(())
}

async fn analyze_table(client: &Client, schema: &str, table: &str) -> Result<()> {
    let sql = format!("ANALYZE \"{}\".\"{}\"", schema, table);
    client
        .execute(&sql, &[])
        .await
        .map_err(|e| anyhow::anyhow!("ANALYZE failed: {}", e))?;
    Ok(())
}

async fn freeze_table(client: &Client, schema: &str, table: &str) -> Result<()> {
    // INDEX_CLEANUP FALSE avoids index bloat during aggressive freeze passes.
    // VERBOSE surfaces progress notices to the PostgreSQL log.
    let sql = format!(
        "VACUUM (VERBOSE, FREEZE, INDEX_CLEANUP FALSE) \"{}\".\"{}\"",
        schema, table
    );
    client
        .execute(&sql, &[])
        .await
        .map_err(|e| anyhow::anyhow!("VACUUM FREEZE failed: {}", e))?;
    Ok(())
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
    force: bool,
    dry_run: bool,
    logger: &Arc<Logger>,
    summary: &mut OperationSummary,
) -> Result<bool> {
    let active_sessions = find_active_vacuums(client, schema, table).await?;
    if active_sessions.is_empty() {
        return Ok(true); // no conflict — proceed
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
        if dry_run {
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
        if force {
            if dry_run {
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
const BACKEND_TYPE_AUTOVACUUM_WORKER: &str = "autovacuum worker";

/// Vacuum all tables that have never been vacuumed.
/// If `force` is true, active vacuums on the table are terminated before starting.
/// Otherwise tables with an active vacuum are skipped.
pub async fn run_vacuum_never_vacuumed(
    client: &Client,
    tables: &[TableInfo],
    dry_run: bool,
    force: bool,
    logger: &Arc<Logger>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary::default();
    summary.total = tables.len();

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No never-vacuumed tables found.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} never-vacuumed table(s).", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            force,
            dry_run,
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
                    "[DRY RUN] Would run: VACUUM \"{}\".\"{}\"  (live={}, dead={})",
                    t.schema_name, t.table_name, t.n_live_tup, t.n_dead_tup
                ),
            );
            continue;
        }

        logger.log_table_start(i + 1, tables.len(), &t.schema_name, &t.table_name, OP_VACUUM);
        let start = Instant::now();
        match vacuum_table(client, &t.schema_name, &t.table_name).await {
            Ok(()) => {
                logger.log_table_success(&t.schema_name, &t.table_name, OP_VACUUM, start.elapsed());
                summary.succeeded += 1;
            }
            Err(e) => {
                let reason = e.to_string();
                if is_lock_timeout(&reason) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(&t.schema_name, &t.table_name, OP_VACUUM, &reason);
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
pub async fn run_analyze_never_analyzed(
    client: &Client,
    tables: &[TableInfo],
    dry_run: bool,
    force: bool,
    logger: &Arc<Logger>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary::default();
    summary.total = tables.len();

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No never-analyzed tables found.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} never-analyzed table(s).", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            force,
            dry_run,
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

        logger.log_table_start(i + 1, tables.len(), &t.schema_name, &t.table_name, OP_ANALYZE);
        let start = Instant::now();
        match analyze_table(client, &t.schema_name, &t.table_name).await {
            Ok(()) => {
                logger.log_table_success(&t.schema_name, &t.table_name, OP_ANALYZE, start.elapsed());
                summary.succeeded += 1;
            }
            Err(e) => {
                let reason = e.to_string();
                if is_lock_timeout(&reason) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(&t.schema_name, &t.table_name, OP_ANALYZE, &reason);
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
    dry_run: bool,
    force: bool,
    logger: &Arc<Logger>,
) -> Result<OperationSummary> {

    let mut summary = OperationSummary::default();
    summary.total = tables.len();

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
        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            force,
            dry_run,
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
                    "[DRY RUN] Would run: VACUUM (VERBOSE, FREEZE, INDEX_CLEANUP FALSE) \"{}\".\"{}\"",
                    t.schema_name, t.table_name
                ),
            );
            continue;
        }

        logger.log_table_start(i + 1, tables.len(), &t.schema_name, &t.table_name, OP_FREEZE);
        let start = Instant::now();
        match freeze_table(client, &t.schema_name, &t.table_name).await {
            Ok(()) => {
                logger.log_table_success(&t.schema_name, &t.table_name, OP_FREEZE, start.elapsed());
                summary.succeeded += 1;
            }
            Err(e) => {
                let reason = e.to_string();
                if is_lock_timeout(&reason) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(&t.schema_name, &t.table_name, OP_FREEZE, &reason);
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
pub async fn run_bloat_vacuum(
    client: &Client,
    tables: &[BloatTableInfo],
    dry_run: bool,
    force: bool,
    already_handled: &std::collections::HashSet<(String, String)>,
    logger: &Arc<Logger>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary::default();
    summary.total = tables.len();

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

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            force,
            dry_run,
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
        match vacuum_table(client, &t.schema_name, &t.table_name).await {
            Ok(()) => {
                logger.log_table_success(&t.schema_name, &t.table_name, OP_BLOAT, start.elapsed());
                summary.succeeded += 1;
            }
            Err(e) => {
                let reason = e.to_string();
                if is_lock_timeout(&reason) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(&t.schema_name, &t.table_name, OP_BLOAT, &reason);
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
pub async fn run_stale_stats_analyze(
    client: &Client,
    tables: &[crate::types::StaleStatsTableInfo],
    analyze_threshold: i64,
    analyze_scale_factor: f64,
    dry_run: bool,
    force: bool,
    already_handled: &std::collections::HashSet<(String, String)>,
    logger: &Arc<Logger>,
) -> Result<OperationSummary> {
    let mut summary = OperationSummary::default();
    summary.total = tables.len();

    if tables.is_empty() {
        logger.log(LogLevel::Success, "No stale-stats candidates found.");
        return Ok(summary);
    }

    logger.log(
        LogLevel::Info,
        &format!("Found {} stale-stats candidate(s).", tables.len()),
    );

    for (i, t) in tables.iter().enumerate() {
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

        let proceed = handle_active_vacuums(
            client,
            &t.schema_name,
            &t.table_name,
            force,
            dry_run,
            logger,
            &mut summary,
        )
        .await?;

        if !proceed {
            continue;
        }

        if dry_run {
            let effective_threshold = t.effective_threshold(analyze_threshold, analyze_scale_factor);
            logger.log(
                LogLevel::Info,
                &format!(
                    "[DRY RUN] Would run: ANALYZE \"{}\".\"{}\"  (mods={}, threshold={})",
                    t.schema_name, t.table_name, t.n_mod_since_analyze, effective_threshold
                ),
            );
            continue;
        }

        logger.log_table_start(i + 1, tables.len(), &t.schema_name, &t.table_name, "ANALYZE (STALE STATS)");
        let start = Instant::now();
        match analyze_table(client, &t.schema_name, &t.table_name).await {
            Ok(()) => {
                logger.log_table_success(&t.schema_name, &t.table_name, "ANALYZE (STALE STATS)", start.elapsed());
                summary.succeeded += 1;
            }
            Err(e) => {
                let reason = e.to_string();
                if is_lock_timeout(&reason) {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Skipping \"{}\".\"{}\" — could not acquire lock within 10ms",
                            t.schema_name, t.table_name
                        ),
                    );
                    summary.skipped += 1;
                } else {
                    logger.log_table_failed(&t.schema_name, &t.table_name, "ANALYZE (STALE STATS)", &reason);
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
        .map_err(|e| anyhow::anyhow!("Failed to discover schemas: {}", e))?;

    Ok(rows.into_iter().map(|row| row.get::<_, String>(0)).collect())
}
