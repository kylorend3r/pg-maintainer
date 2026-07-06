use pg_maintainer::config::{DEFAULT_BLOAT_THRESHOLD_PCT, DEFAULT_MAINTENANCE_WORK_MEM_GB, DEFAULT_WRAPAROUND_MIN_AGE};
use pg_maintainer::connection::{self, ConnectionConfig};
use pg_maintainer::logging::{LogLevel, Logger};
use pg_maintainer::operations;
use pg_maintainer::types::{LogFormat, Mode, SslMode};
use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

// ─── CLI args ─────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "pg-maintainer — PostgreSQL table maintenance: vacuum, analyze, and anti-wraparound freeze",
    long_about = None
)]
struct Args {
    /// PostgreSQL host (or PG_HOST env var)
    #[arg(short = 'H', long)]
    host: Option<String>,

    /// PostgreSQL port (or PG_PORT env var)
    #[arg(short, long)]
    port: Option<u16>,

    /// Database name (or PG_DATABASE env var)
    #[arg(short, long)]
    database: Option<String>,

    /// PostgreSQL username (or PG_USER env var)
    #[arg(short = 'U', long)]
    username: Option<String>,

    /// Password (prefer PG_PASSWORD env var — CLI value is visible in process lists)
    #[arg(
        short = 'P',
        long,
        help = "Password. INSECURE: prefer PG_PASSWORD env var."
    )]
    password: Option<String>,

    /// Comma-separated schema names to maintain
    #[arg(
        short = 's',
        long,
        help = "Comma-separated schema names. Mutually exclusive with --discover-all-schemas."
    )]
    schema: Option<String>,

    /// Discover and maintain all user schemas (excludes system schemas)
    #[arg(long, default_value = "false")]
    discover_all_schemas: bool,

    /// Limit all phases to a single table name.
    /// The table must exist in at least one of the specified schemas.
    #[arg(short = 't', long, help = "Limit maintenance to a single table name")]
    table: Option<String>,

    /// Show what would be done without executing any maintenance commands
    #[arg(short = 'f', long, default_value = "false")]
    dry_run: bool,

    // ── Mode selection ───────────────────────────────────────────────────────
    /// Comma-separated modes to run: vacuum,analyze,freeze,bloat.
    /// Defaults to all four when omitted.
    #[arg(long, value_delimiter = ',', help = "Modes to run: vacuum, analyze, freeze, bloat")]
    mode: Option<Vec<String>>,

    /// Terminate active vacuum/autovacuum on each table before maintaining it.
    /// Without --force, tables with an active vacuum are skipped instead.
    #[arg(long, default_value = "false")]
    force: bool,

    // ── Bloat tuning ─────────────────────────────────────────────────────────
    /// Bloat threshold percentage (default: 80.0). Tables with dead tuple ratio
    /// exceeding this percentage are considered bloat candidates.
    #[arg(long, default_value_t = DEFAULT_BLOAT_THRESHOLD_PCT)]
    bloat_threshold_pct: f64,

    // ── Size filtering ───────────────────────────────────────────────────────
    /// Minimum table size in GB. Tables smaller than this are excluded.
    #[arg(long, value_name = "GB", help = "Minimum table size in GB (default: 0, no floor)")]
    min_table_size_gb: Option<f64>,

    /// Maximum table size in GB. Tables larger than this are excluded.
    #[arg(long, value_name = "GB", help = "Maximum table size in GB (default: none, no ceiling)")]
    max_table_size_gb: Option<f64>,

    // ── Freeze tuning ────────────────────────────────────────────────────────
    /// Minimum XID age to flag a table as a wraparound candidate.
    /// Defaults to autovacuum_freeze_max_age (200 000 000).
    /// Ignored when --wraparound-pct is set.
    #[arg(
        long,
        default_value_t = DEFAULT_WRAPAROUND_MIN_AGE,
        help = "Minimum XID age threshold for wraparound candidates (default: 200000000)"
    )]
    wraparound_min_age: i64,

    /// Flag tables that have consumed this percentage of autovacuum_freeze_max_age.
    /// Overrides --wraparound-min-age when set. Example: 75 flags tables at 75% toward wraparound.
    #[arg(
        long,
        value_name = "PCT",
        help = "Wraparound threshold as % of autovacuum_freeze_max_age (0–100). Overrides --wraparound-min-age."
    )]
    wraparound_pct: Option<f64>,

    // ── Maintenance memory ───────────────────────────────────────────────────
    /// maintenance_work_mem in GB for this session (default: 1, max: 32)
    #[arg(short = 'w', long, default_value_t = DEFAULT_MAINTENANCE_WORK_MEM_GB)]
    maintenance_work_mem_gb: u64,

    // ── SSL ──────────────────────────────────────────────────────────────────
    #[arg(long, default_value = "disable", value_parser = clap::value_parser!(SslMode))]
    sslmode: SslMode,

    #[arg(long, help = "Path to CA certificate (.pem) for SSL")]
    ssl_ca_cert: Option<String>,

    #[arg(long, help = "Path to client certificate (.pem). Requires --ssl-client-key.")]
    ssl_client_cert: Option<String>,

    #[arg(long, help = "Path to client private key (.pem). Requires --ssl-client-cert.")]
    ssl_client_key: Option<String>,

    // ── Logging ──────────────────────────────────────────────────────────────
    #[arg(short = 'l', long, default_value = "maintainer.log")]
    log_file: String,

    #[arg(long, default_value = "text", value_parser = clap::value_parser!(LogFormat))]
    log_format: LogFormat,

    /// Suppress terminal output; all logs still go to the log file
    #[arg(long, default_value = "false")]
    silence_mode: bool,

    // ── Config file ──────────────────────────────────────────────────────────
    /// Path to a TOML configuration file. CLI arguments take precedence.
    #[arg(short = 'C', long, value_name = "FILE")]
    config: Option<String>,
}

// ─── TOML config ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct Config {
    host: Option<String>,
    port: Option<u16>,
    database: Option<String>,
    username: Option<String>,
    password: Option<String>,
    schema: Option<String>,
    discover_all_schemas: Option<bool>,
    table: Option<String>,
    dry_run: Option<bool>,
    mode: Option<Vec<String>>,
    force: Option<bool>,
    bloat_threshold_pct: Option<f64>,
    min_table_size_gb: Option<f64>,
    max_table_size_gb: Option<f64>,
    wraparound_min_age: Option<i64>,
    wraparound_pct: Option<f64>,
    maintenance_work_mem_gb: Option<u64>,
    sslmode: Option<String>,
    ssl_ca_cert: Option<String>,
    ssl_client_cert: Option<String>,
    ssl_client_key: Option<String>,
    log_file: Option<String>,
    log_format: Option<String>,
    silence_mode: Option<bool>,
}

fn resolve_env_interpolation(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        if let Some(var_name) = v.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
            std::env::var(var_name).ok()
        } else {
            Some(v)
        }
    })
}

fn load_config_file(path: &str) -> Result<Config> {
    let file_path = Path::new(path);
    if !file_path.exists() {
        return Err(anyhow::anyhow!("Configuration file not found: {}", path));
    }
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read configuration file: {}", path))?;
    let mut cfg = toml::from_str::<Config>(&content)
        .with_context(|| format!("Failed to parse TOML configuration file: {}", path))?;
    cfg.password = resolve_env_interpolation(cfg.password);
    Ok(cfg)
}

/// Merge TOML config into args. CLI args always win (only fill in unset fields).
fn merge_config(file: Config, mut args: Args) -> Args {
    if args.host.is_none() { args.host = file.host; }
    if args.port.is_none() { args.port = file.port; }
    if args.database.is_none() { args.database = file.database; }
    if args.username.is_none() { args.username = file.username; }
    if args.password.is_none() { args.password = file.password; }
    if args.schema.is_none() { args.schema = file.schema; }

    if !args.discover_all_schemas {
        args.discover_all_schemas = file.discover_all_schemas.unwrap_or(false);
    }
    if args.table.is_none() { args.table = file.table; }
    if !args.dry_run { args.dry_run = file.dry_run.unwrap_or(false); }
    if args.mode.is_none() { args.mode = file.mode; }
    if !args.force { args.force = file.force.unwrap_or(false); }

    if args.bloat_threshold_pct == DEFAULT_BLOAT_THRESHOLD_PCT {
        if let Some(v) = file.bloat_threshold_pct { args.bloat_threshold_pct = v; }
    }
    if args.min_table_size_gb.is_none() { args.min_table_size_gb = file.min_table_size_gb; }
    if args.max_table_size_gb.is_none() { args.max_table_size_gb = file.max_table_size_gb; }

    if args.wraparound_min_age == DEFAULT_WRAPAROUND_MIN_AGE {
        if let Some(v) = file.wraparound_min_age { args.wraparound_min_age = v; }
    }
    if args.wraparound_pct.is_none() { args.wraparound_pct = file.wraparound_pct; }
    if args.maintenance_work_mem_gb == DEFAULT_MAINTENANCE_WORK_MEM_GB {
        if let Some(v) = file.maintenance_work_mem_gb { args.maintenance_work_mem_gb = v; }
    }

    if args.sslmode == SslMode::Disable {
        if let Some(ref s) = file.sslmode {
            if let Ok(m) = s.parse::<SslMode>() { args.sslmode = m; }
        }
    }
    if args.ssl_ca_cert.is_none() { args.ssl_ca_cert = file.ssl_ca_cert; }
    if args.ssl_client_cert.is_none() { args.ssl_client_cert = file.ssl_client_cert; }
    if args.ssl_client_key.is_none() { args.ssl_client_key = file.ssl_client_key; }

    if args.log_file == "maintainer.log" {
        if let Some(lf) = file.log_file { args.log_file = lf; }
    }
    if args.log_format == LogFormat::Text {
        if let Some(ref lf) = file.log_format {
            if let Ok(f) = lf.parse::<LogFormat>() { args.log_format = f; }
        }
    }
    if !args.silence_mode { args.silence_mode = file.silence_mode.unwrap_or(false); }

    args
}

// ─── Entry point ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::parse();

    if args.password.is_some() {
        eprintln!(
            "Warning: passing --password on the command line is insecure \
             (visible in process list and shell history). \
             Use the PG_PASSWORD environment variable instead."
        );
    }

    // Load and merge TOML config if provided
    if let Some(ref path) = args.config.clone() {
        let file_cfg = load_config_file(path).context("Failed to load configuration file")?;
        args = merge_config(file_cfg, args);
    }

    // Validate: need either --schema or --discover-all-schemas
    if args.schema.is_none() && !args.discover_all_schemas {
        return Err(anyhow::anyhow!(
            "Either --schema or --discover-all-schemas must be provided"
        ));
    }

    // Parse and validate modes
    let enabled_modes: HashSet<Mode> = if let Some(ref mode_strs) = args.mode {
        let mut modes = HashSet::new();
        for mode_str in mode_strs {
            let mode: Mode = mode_str.parse()
                .map_err(|e: String| anyhow::anyhow!("Invalid mode: {}", e))?;
            modes.insert(mode);
        }
        if modes.is_empty() {
            return Err(anyhow::anyhow!("--mode list cannot be empty"));
        }
        modes
    } else {
        [Mode::Vacuum, Mode::Analyze, Mode::Freeze, Mode::Bloat]
            .iter()
            .copied()
            .collect()
    };

    // Validate bloat_threshold_pct range
    if !(0.0..=100.0).contains(&args.bloat_threshold_pct) {
        return Err(anyhow::anyhow!(
            "--bloat-threshold-pct ({}) must be between 0 and 100",
            args.bloat_threshold_pct
        ));
    }

    // Validate and convert size filters (GB to bytes)
    let min_gb = args.min_table_size_gb.unwrap_or(0.0);
    let max_gb = args.max_table_size_gb.unwrap_or(f64::INFINITY);
    if min_gb < 0.0 || max_gb < 0.0 {
        return Err(anyhow::anyhow!(
            "--min-table-size-gb and --max-table-size-gb must be >= 0"
        ));
    }
    if min_gb > max_gb && max_gb != f64::INFINITY {
        return Err(anyhow::anyhow!(
            "--min-table-size-gb ({}) must be <= --max-table-size-gb ({})",
            min_gb, max_gb
        ));
    }
    let min_bytes = (min_gb * 1_073_741_824.0) as i64;
    let max_bytes = if max_gb == f64::INFINITY {
        i64::MAX
    } else {
        (max_gb * 1_073_741_824.0) as i64
    };

    // Validate maintenance_work_mem_gb
    if args.maintenance_work_mem_gb > pg_maintainer::config::MAX_MAINTENANCE_WORK_MEM_GB {
        return Err(anyhow::anyhow!(
            "--maintenance-work-mem-gb ({}) exceeds maximum ({})",
            args.maintenance_work_mem_gb,
            pg_maintainer::config::MAX_MAINTENANCE_WORK_MEM_GB
        ));
    }

    // Validate wraparound_pct range
    if let Some(pct) = args.wraparound_pct {
        if !(0.0..=100.0).contains(&pct) {
            return Err(anyhow::anyhow!(
                "--wraparound-pct ({}) must be between 0 and 100",
                pct
            ));
        }
    }

    let logger = Arc::new(Logger::new(
        args.log_file.clone(),
        args.silence_mode,
        args.log_format,
    ));

    if args.silence_mode {
        println!(
            "Starting pg-maintainer (silence mode — logs: {})",
            args.log_file
        );
    }

    // Build connection config
    let conn_cfg = ConnectionConfig::from_args(
        args.host.clone(),
        args.port,
        args.database.clone(),
        args.username.clone(),
        args.password.clone(),
        args.sslmode,
        args.ssl_ca_cert.clone(),
        args.ssl_client_cert.clone(),
        args.ssl_client_key.clone(),
    )?;

    let conn_string = conn_cfg.build_connection_string();

    logger.log(
        LogLevel::Info,
        &format!(
            "Connecting to database '{}' at {}:{}",
            conn_cfg.database, conn_cfg.host, conn_cfg.port
        ),
    );

    let client = connection::connect(
        &conn_string,
        &conn_cfg.sslmode,
        conn_cfg.ssl_ca_cert.clone(),
        conn_cfg.ssl_client_cert.clone(),
        conn_cfg.ssl_client_key.clone(),
    )
    .await
    .context("Failed to connect to PostgreSQL")?;

    logger.log(
        LogLevel::Success,
        &format!("Connected to database '{}'", conn_cfg.database),
    );

    // Set maintenance_work_mem
    connection::set_maintenance_work_mem(&client, args.maintenance_work_mem_gb)
        .await
        .context("Failed to set maintenance_work_mem")?;
    logger.log(
        LogLevel::Info,
        &format!(
            "maintenance_work_mem set to {}GB for this session",
            args.maintenance_work_mem_gb
        ),
    );

    // Set vacuum_buffer_usage_limit to 1/16 of shared_buffers (PostgreSQL 16+).
    match connection::set_vacuum_buffer_usage_limit(&client).await {
        Ok((shared_buffers_kb, limit_kb)) => logger.log(
            LogLevel::Info,
            &format!(
                "vacuum_buffer_usage_limit set to {} (1/16 of shared_buffers {})",
                connection::format_kb_readable(limit_kb),
                connection::format_kb_readable(shared_buffers_kb),
            ),
        ),
        Err(e) => logger.log(
            LogLevel::Warning,
            &format!(
                "Could not set vacuum_buffer_usage_limit (requires PostgreSQL 16+): {}",
                e
            ),
        ),
    }

    // Set lock_timeout so VACUUM/ANALYZE fail fast instead of blocking indefinitely.
    connection::set_lock_timeout(&client)
        .await
        .context("Failed to set lock_timeout")?;
    logger.log(LogLevel::Info, "lock_timeout set to 10ms for this session");

    // Resolve schemas
    let schemas: Vec<String> = if args.discover_all_schemas && args.schema.is_none() {
        logger.log(LogLevel::Info, "Discovering all user schemas...");
        let discovered = operations::discover_all_user_schemas(&client).await?;
        if discovered.is_empty() {
            return Err(anyhow::anyhow!(
                "No user schemas found. System schemas are excluded."
            ));
        }
        logger.log(
            LogLevel::Success,
            &format!(
                "Discovered {} schema(s): {}",
                discovered.len(),
                discovered.join(", ")
            ),
        );
        discovered
    } else {
        args.schema
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    if schemas.is_empty() {
        return Err(anyhow::anyhow!("No schemas to process."));
    }

    if args.dry_run {
        logger.log(
            LogLevel::Warning,
            "DRY RUN MODE — no maintenance commands will be executed.",
        );
    }

    let table_filter = args.table.as_deref();
    if let Some(tbl) = table_filter {
        logger.log(
            LogLevel::Info,
            &format!("Table filter active — limiting all phases to table \"{}\"", tbl),
        );
    }

    // Resolve the effective wraparound min-age for Phase 3.
    // If --wraparound-pct is given, convert it to an absolute XID age using the
    // server's autovacuum_freeze_max_age; otherwise use --wraparound-min-age as-is.
    let effective_wraparound_min_age = if let Some(pct) = args.wraparound_pct {
        let freeze_max_age = operations::get_freeze_max_age(&client).await?;
        let computed = (pct / 100.0 * freeze_max_age as f64) as i64;
        logger.log(
            LogLevel::Info,
            &format!(
                "Wraparound threshold: {:.1}% of freeze_max_age {} = {} XID age",
                pct, freeze_max_age, computed
            ),
        );
        computed
    } else {
        args.wraparound_min_age
    };

    let start = std::time::Instant::now();

    let mut already_handled: HashSet<(String, String)> = HashSet::new();

    // ── Phase 1: VACUUM never-vacuumed tables ─────────────────────────────────
    let vacuum_summary = if enabled_modes.contains(&Mode::Vacuum) {
        logger.log(LogLevel::Info, "═══ Phase 1: VACUUM (never vacuumed) ═══");
        let summary = operations::run_vacuum_never_vacuumed(&client, &schemas, table_filter, args.dry_run, args.force, min_bytes, max_bytes, &logger)
            .await
            .context("VACUUM phase failed")?;
        for t in operations::find_never_vacuumed(&client, &schemas, table_filter, min_bytes, max_bytes).await.unwrap_or_default().iter() {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        summary
    } else {
        logger.log(LogLevel::Info, "Skipping Phase 1: VACUUM (not in --mode)");
        Default::default()
    };

    // ── Phase 2: ANALYZE never-analyzed tables ────────────────────────────────
    let analyze_summary = if enabled_modes.contains(&Mode::Analyze) {
        logger.log(LogLevel::Info, "═══ Phase 2: ANALYZE (never analyzed) ═══");
        let summary = operations::run_analyze_never_analyzed(&client, &schemas, table_filter, args.dry_run, args.force, min_bytes, max_bytes, &logger)
            .await
            .context("ANALYZE phase failed")?;
        for t in operations::find_never_analyzed(&client, &schemas, table_filter, min_bytes, max_bytes).await.unwrap_or_default().iter() {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        summary
    } else {
        logger.log(LogLevel::Info, "Skipping Phase 2: ANALYZE (not in --mode)");
        Default::default()
    };

    // ── Phase 3: VACUUM FREEZE wraparound candidates ──────────────────────────
    let freeze_summary = if enabled_modes.contains(&Mode::Freeze) {
        logger.log(
            LogLevel::Info,
            "═══ Phase 3: VACUUM FREEZE (wraparound candidates) ═══",
        );
        let summary = operations::run_freeze_wraparound(
            &client,
            &schemas,
            table_filter,
            effective_wraparound_min_age,
            args.dry_run,
            args.force,
            min_bytes,
            max_bytes,
            &logger,
        )
        .await
        .context("VACUUM FREEZE phase failed")?;
        for t in operations::find_wraparound_candidates(&client, &schemas, effective_wraparound_min_age, table_filter, min_bytes, max_bytes).await.unwrap_or_default().iter() {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        summary
    } else {
        logger.log(LogLevel::Info, "Skipping Phase 3: VACUUM FREEZE (not in --mode)");
        Default::default()
    };

    // ── Phase 4: VACUUM bloat candidates ─────────────────────────────────────
    let bloat_summary = if enabled_modes.contains(&Mode::Bloat) {
        logger.log(LogLevel::Info, "═══ Phase 4: VACUUM (bloat) ═══");
        operations::run_bloat_vacuum(
            &client,
            &schemas,
            table_filter,
            args.bloat_threshold_pct,
            pg_maintainer::config::DEFAULT_BLOAT_MIN_DEAD_TUP,
            args.dry_run,
            args.force,
            min_bytes,
            max_bytes,
            &already_handled,
            &logger,
        )
        .await
        .context("VACUUM BLOAT phase failed")?
    } else {
        logger.log(LogLevel::Info, "Skipping Phase 4: VACUUM (bloat) (not in --mode)");
        Default::default()
    };

    // ── Final summary ─────────────────────────────────────────────────────────
    let elapsed = start.elapsed();
    let total_tables =
        vacuum_summary.total + analyze_summary.total + freeze_summary.total + bloat_summary.total;
    let total_ok =
        vacuum_summary.succeeded + analyze_summary.succeeded + freeze_summary.succeeded + bloat_summary.succeeded;
    let total_fail =
        vacuum_summary.failed + analyze_summary.failed + freeze_summary.failed + bloat_summary.failed;

    logger.log_always(
        LogLevel::Success,
        &format!(
            "pg-maintainer completed in {:.2?} — \
             tables processed: {} | succeeded: {} | failed: {}",
            elapsed, total_tables, total_ok, total_fail
        ),
    );

    if enabled_modes.contains(&Mode::Vacuum) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  VACUUM        — total: {}, ok: {}, failed: {}, skipped: {}",
                vacuum_summary.total, vacuum_summary.succeeded, vacuum_summary.failed, vacuum_summary.skipped
            ),
        );
    }
    if enabled_modes.contains(&Mode::Analyze) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  ANALYZE       — total: {}, ok: {}, failed: {}, skipped: {}",
                analyze_summary.total, analyze_summary.succeeded, analyze_summary.failed, analyze_summary.skipped
            ),
        );
    }
    if enabled_modes.contains(&Mode::Freeze) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  VACUUM FREEZE — total: {}, ok: {}, failed: {}, skipped: {}",
                freeze_summary.total, freeze_summary.succeeded, freeze_summary.failed, freeze_summary.skipped
            ),
        );
    }
    if enabled_modes.contains(&Mode::Bloat) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  VACUUM (bloat) — total: {}, ok: {}, failed: {}, skipped: {}",
                bloat_summary.total, bloat_summary.succeeded, bloat_summary.failed, bloat_summary.skipped
            ),
        );
    }

    if total_fail > 0 {
        return Err(anyhow::anyhow!(
            "pg-maintainer completed with {} error(s)",
            total_fail
        ));
    }

    Ok(())
}
