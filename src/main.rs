use anyhow::{Context, Result};
use clap::Parser;
use pg_maintainer::config::{
    DEFAULT_BLOAT_THRESHOLD_PCT, DEFAULT_MAINTENANCE_WORK_MEM_GB, DEFAULT_WRAPAROUND_MIN_AGE,
};
use pg_maintainer::connection::{self, ConnectionConfig};
use pg_maintainer::logging::{LogLevel, Logger};
use pg_maintainer::operations;
use pg_maintainer::types::{LogFormat, Mode, SslMode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::watch;

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
    /// Comma-separated modes to run: never-vacuumed, never-analyzed, wraparound, bloated, stale-stats.
    /// Defaults to all five when omitted.
    #[arg(
        long,
        value_delimiter = ',',
        help = "Modes to run: never-vacuumed, never-analyzed, wraparound, bloated, stale-stats"
    )]
    mode: Option<Vec<String>>,

    /// Terminate a conflicting manual VACUUM before starting (autovacuum workers are always terminated automatically).
    #[arg(long, default_value = "false")]
    force: bool,

    /// Cap each mode to its top N candidate tables (default: no limit).
    #[arg(long, help = "Limit each mode to top N tables (default: unlimited)")]
    limit: Option<i64>,

    // ── Bloat tuning ─────────────────────────────────────────────────────────
    /// Bloat threshold percentage (default: 80.0). Tables with dead tuple ratio
    /// exceeding this percentage are considered bloat candidates.
    #[arg(long, default_value_t = DEFAULT_BLOAT_THRESHOLD_PCT)]
    bloat_threshold_pct: f64,

    // ── Stale-stats tuning ───────────────────────────────────────────────────
    /// Flat modification-count floor before a table is considered for re-analysis.
    /// Defaults to the connected server's autovacuum_analyze_threshold if omitted.
    #[arg(
        long,
        help = "Modification-count floor for stale-stats (default: read from server's autovacuum_analyze_threshold)"
    )]
    analyze_threshold: Option<i64>,

    /// Scale factor applied to live row count when computing the re-analyze threshold.
    /// Defaults to the connected server's autovacuum_analyze_scale_factor if omitted.
    #[arg(
        long,
        help = "Scale factor for stale-stats (default: read from server's autovacuum_analyze_scale_factor)"
    )]
    analyze_scale_factor: Option<f64>,

    // ── Size filtering ───────────────────────────────────────────────────────
    /// Minimum table size in GB. Tables smaller than this are excluded.
    #[arg(
        long,
        value_name = "GB",
        help = "Minimum table size in GB (default: 0, no floor)"
    )]
    min_table_size_gb: Option<f64>,

    /// Maximum table size in GB. Tables larger than this are excluded.
    #[arg(
        long,
        value_name = "GB",
        help = "Maximum table size in GB (default: none, no ceiling)"
    )]
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

    #[arg(
        long,
        help = "Path to client certificate (.pem). Requires --ssl-client-key."
    )]
    ssl_client_cert: Option<String>,

    #[arg(
        long,
        help = "Path to client private key (.pem). Requires --ssl-client-cert."
    )]
    ssl_client_key: Option<String>,

    // ── Logging ──────────────────────────────────────────────────────────────
    #[arg(short = 'l', long, default_value = "maintainer.log")]
    log_file: String,

    #[arg(long, default_value = "text", value_parser = clap::value_parser!(LogFormat))]
    log_format: LogFormat,

    /// Suppress terminal output; all logs still go to the log file
    #[arg(long, default_value = "false")]
    silence_mode: bool,

    // ── Timeouts ─────────────────────────────────────────────────────────────
    /// Statement timeout in seconds for each VACUUM/ANALYZE operation (default: 0 = unbounded).
    /// Set this for unattended runs to prevent VACUUM from running indefinitely if it gets stuck.
    #[arg(long, default_value = "0")]
    statement_timeout_seconds: u64,

    /// TCP connection timeout in seconds (default: 10).
    /// A network partition can hang startup for the OS default; this bounds that.
    #[arg(long, default_value = "10")]
    connect_timeout_seconds: u64,

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
    limit: Option<i64>,
    bloat_threshold_pct: Option<f64>,
    analyze_threshold: Option<i64>,
    analyze_scale_factor: Option<f64>,
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
    statement_timeout_seconds: Option<u64>,
    connect_timeout_seconds: Option<u64>,
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
    if args.host.is_none() {
        args.host = file.host;
    }
    if args.port.is_none() {
        args.port = file.port;
    }
    if args.database.is_none() {
        args.database = file.database;
    }
    if args.username.is_none() {
        args.username = file.username;
    }
    if args.password.is_none() {
        args.password = file.password;
    }
    if args.schema.is_none() {
        args.schema = file.schema;
    }

    if !args.discover_all_schemas {
        args.discover_all_schemas = file.discover_all_schemas.unwrap_or(false);
    }
    if args.table.is_none() {
        args.table = file.table;
    }
    if !args.dry_run {
        args.dry_run = file.dry_run.unwrap_or(false);
    }
    if args.mode.is_none() {
        args.mode = file.mode;
    }
    if !args.force {
        args.force = file.force.unwrap_or(false);
    }
    if args.limit.is_none() {
        args.limit = file.limit;
    }

    if args.bloat_threshold_pct == DEFAULT_BLOAT_THRESHOLD_PCT {
        if let Some(v) = file.bloat_threshold_pct {
            args.bloat_threshold_pct = v;
        }
    }
    if args.analyze_threshold.is_none() {
        args.analyze_threshold = file.analyze_threshold;
    }
    if args.analyze_scale_factor.is_none() {
        args.analyze_scale_factor = file.analyze_scale_factor;
    }
    if args.min_table_size_gb.is_none() {
        args.min_table_size_gb = file.min_table_size_gb;
    }
    if args.max_table_size_gb.is_none() {
        args.max_table_size_gb = file.max_table_size_gb;
    }

    if args.wraparound_min_age == DEFAULT_WRAPAROUND_MIN_AGE {
        if let Some(v) = file.wraparound_min_age {
            args.wraparound_min_age = v;
        }
    }
    if args.wraparound_pct.is_none() {
        args.wraparound_pct = file.wraparound_pct;
    }
    if args.maintenance_work_mem_gb == DEFAULT_MAINTENANCE_WORK_MEM_GB {
        if let Some(v) = file.maintenance_work_mem_gb {
            args.maintenance_work_mem_gb = v;
        }
    }

    if args.sslmode == SslMode::Disable {
        if let Some(ref s) = file.sslmode {
            if let Ok(m) = s.parse::<SslMode>() {
                args.sslmode = m;
            }
        }
    }
    if args.ssl_ca_cert.is_none() {
        args.ssl_ca_cert = file.ssl_ca_cert;
    }
    if args.ssl_client_cert.is_none() {
        args.ssl_client_cert = file.ssl_client_cert;
    }
    if args.ssl_client_key.is_none() {
        args.ssl_client_key = file.ssl_client_key;
    }

    if args.log_file == "maintainer.log" {
        if let Some(lf) = file.log_file {
            args.log_file = lf;
        }
    }
    if args.log_format == LogFormat::Text {
        if let Some(ref lf) = file.log_format {
            if let Ok(f) = lf.parse::<LogFormat>() {
                args.log_format = f;
            }
        }
    }
    if !args.silence_mode {
        args.silence_mode = file.silence_mode.unwrap_or(false);
    }

    if args.statement_timeout_seconds == 0 {
        if let Some(v) = file.statement_timeout_seconds {
            args.statement_timeout_seconds = v;
        }
    }
    if args.connect_timeout_seconds == 10 {
        if let Some(v) = file.connect_timeout_seconds {
            args.connect_timeout_seconds = v;
        }
    }

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
            let mode: Mode = mode_str
                .parse()
                .map_err(|e: String| anyhow::anyhow!("Invalid mode: {}", e))?;
            modes.insert(mode);
        }
        if modes.is_empty() {
            return Err(anyhow::anyhow!("--mode list cannot be empty"));
        }
        modes
    } else {
        [
            Mode::NeverVacuumed,
            Mode::NeverAnalyzed,
            Mode::Wraparound,
            Mode::Bloated,
            Mode::StaleStats,
        ]
        .iter()
        .copied()
        .collect()
    };

    // Validate --limit
    if let Some(limit) = args.limit {
        if limit <= 0 {
            return Err(anyhow::anyhow!("--limit ({}) must be > 0", limit));
        }
    }

    // Validate --analyze-threshold and --analyze-scale-factor
    if let Some(threshold) = args.analyze_threshold {
        if threshold < 0 {
            return Err(anyhow::anyhow!(
                "--analyze-threshold ({}) must be >= 0",
                threshold
            ));
        }
    }
    if let Some(factor) = args.analyze_scale_factor {
        if factor < 0.0 {
            return Err(anyhow::anyhow!(
                "--analyze-scale-factor ({}) must be >= 0.0",
                factor
            ));
        }
    }

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
            min_gb,
            max_gb
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

    // Set up graceful shutdown signal handlers (SIGTERM and Ctrl-C)
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let shutdown_tx_term = shutdown_tx.clone();
    let shutdown_tx_int = shutdown_tx.clone();

    tokio::spawn(async move {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sigterm) => {
                sigterm.recv().await;
                let _ = shutdown_tx_term.send(true);
            }
            Err(e) => {
                eprintln!("Failed to setup SIGTERM handler: {}", e);
            }
        }
    });

    tokio::spawn(async move {
        if let Err(e) = tokio::signal::ctrl_c().await {
            eprintln!("Failed to setup SIGINT handler: {}", e);
        } else {
            let _ = shutdown_tx_int.send(true);
        }
    });

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
        args.connect_timeout_seconds,
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
        args.statement_timeout_seconds,
    )
    .await
    .context("Failed to connect to PostgreSQL")?;

    logger.log(
        LogLevel::Success,
        &format!("Connected to database '{}'", conn_cfg.database),
    );

    // Resolve schemas early so we can acquire the advisory lock before any maintenance work
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

    // Try to acquire advisory lock to prevent concurrent pg-maintainer instances
    operations::try_acquire_schema_lock(&client, &schemas)
        .await
        .context("Failed to acquire concurrency guard")?;
    logger.log(
        LogLevel::Info,
        &format!(
            "Acquired advisory lock for schema(s): {}",
            schemas.join(", ")
        ),
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

    // Set max_parallel_maintenance_workers to match the server's max_parallel_workers,
    // so VACUUM's index-cleanup phase can use the full parallel worker pool
    // (no effect on Phase 3/freeze, which runs with INDEX_CLEANUP FALSE).
    match connection::set_max_parallel_maintenance_workers(&client).await {
        Ok(workers) => logger.log(
            LogLevel::Info,
            &format!(
                "max_parallel_maintenance_workers set to {} (matches max_parallel_workers)",
                workers
            ),
        ),
        Err(e) => logger.log(
            LogLevel::Warning,
            &format!("Could not set max_parallel_maintenance_workers: {}", e),
        ),
    }

    // Set lock_timeout so VACUUM/ANALYZE fail fast instead of blocking indefinitely.
    connection::set_lock_timeout(&client)
        .await
        .context("Failed to set lock_timeout")?;
    logger.log(LogLevel::Info, "lock_timeout set to 10ms for this session");

    // Resolve --limit (default to no limit if not specified)
    let limit_n = args.limit.unwrap_or(i64::MAX);

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
            &format!(
                "Table filter active — limiting all phases to table \"{}\"",
                tbl
            ),
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
    let vacuum_summary = if enabled_modes.contains(&Mode::NeverVacuumed) {
        logger.log(LogLevel::Info, "═══ Phase 1: VACUUM (never vacuumed) ═══");
        logger.log(
            LogLevel::Info,
            "Searching for tables that have never been vacuumed...",
        );
        let candidates = operations::find_never_vacuumed(
            &client,
            &schemas,
            table_filter,
            min_bytes,
            max_bytes,
            limit_n,
        )
        .await
        .context("Failed to query never-vacuumed tables")?;
        for t in &candidates {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        operations::run_vacuum_never_vacuumed(
            &client,
            &candidates,
            args.dry_run,
            args.force,
            &logger,
            &mut shutdown_rx,
        )
        .await
        .context("VACUUM phase failed")?
    } else {
        logger.log(LogLevel::Info, "Skipping Phase 1: VACUUM (not in --mode)");
        Default::default()
    };

    // ── Phase 2: ANALYZE never-analyzed tables ────────────────────────────────
    let analyze_summary = if enabled_modes.contains(&Mode::NeverAnalyzed) {
        logger.log(LogLevel::Info, "═══ Phase 2: ANALYZE (never analyzed) ═══");
        logger.log(
            LogLevel::Info,
            "Searching for tables that have never been analyzed...",
        );
        let candidates = operations::find_never_analyzed(
            &client,
            &schemas,
            table_filter,
            min_bytes,
            max_bytes,
            limit_n,
        )
        .await
        .context("Failed to query never-analyzed tables")?;
        for t in &candidates {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        operations::run_analyze_never_analyzed(
            &client,
            &candidates,
            args.dry_run,
            args.force,
            &logger,
            &mut shutdown_rx,
        )
        .await
        .context("ANALYZE phase failed")?
    } else {
        logger.log(LogLevel::Info, "Skipping Phase 2: ANALYZE (not in --mode)");
        Default::default()
    };

    // ── Phase 3: VACUUM FREEZE wraparound candidates ──────────────────────────
    let freeze_summary = if enabled_modes.contains(&Mode::Wraparound) {
        logger.log(
            LogLevel::Info,
            "═══ Phase 3: VACUUM FREEZE (wraparound candidates) ═══",
        );
        let candidates = operations::find_wraparound_candidates(
            &client,
            &schemas,
            effective_wraparound_min_age,
            table_filter,
            min_bytes,
            max_bytes,
            limit_n,
        )
        .await
        .context("Failed to query wraparound candidates")?;
        for t in &candidates {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        operations::run_freeze_wraparound(
            &client,
            &candidates,
            args.dry_run,
            args.force,
            &logger,
            &mut shutdown_rx,
        )
        .await
        .context("VACUUM FREEZE phase failed")?
    } else {
        logger.log(
            LogLevel::Info,
            "Skipping Phase 3: VACUUM FREEZE (not in --mode)",
        );
        Default::default()
    };

    // ── Phase 4: VACUUM bloat candidates ─────────────────────────────────────
    let bloat_summary = if enabled_modes.contains(&Mode::Bloated) {
        logger.log(LogLevel::Info, "═══ Phase 4: VACUUM (bloat) ═══");
        logger.log(
            LogLevel::Info,
            &format!(
                "Searching for bloat candidates (>{:.1}% dead tuples)...",
                args.bloat_threshold_pct
            ),
        );
        let candidates = operations::find_bloat_candidates(
            &client,
            &schemas,
            table_filter,
            args.bloat_threshold_pct,
            pg_maintainer::config::DEFAULT_BLOAT_MIN_DEAD_TUP,
            min_bytes,
            max_bytes,
            limit_n,
        )
        .await
        .context("Failed to query bloat candidates")?;
        let summary = operations::run_bloat_vacuum(
            &client,
            &candidates,
            args.dry_run,
            args.force,
            &already_handled,
            &logger,
            &mut shutdown_rx,
        )
        .await
        .context("VACUUM BLOAT phase failed")?;
        for t in &candidates {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        summary
    } else {
        logger.log(
            LogLevel::Info,
            "Skipping Phase 4: VACUUM (bloat) (not in --mode)",
        );
        Default::default()
    };

    // ── Phase 5: ANALYZE stale-stats candidates ──────────────────────────────
    let stale_stats_summary = if enabled_modes.contains(&Mode::StaleStats) {
        logger.log(LogLevel::Info, "═══ Phase 5: ANALYZE (stale stats) ═══");

        let (server_analyze_threshold, server_analyze_scale_factor) =
            match operations::get_analyze_settings(&client).await {
                Ok(v) => v,
                Err(e) => {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Could not read autovacuum_analyze settings from server, \
                             falling back to defaults ({}, {}): {}",
                            pg_maintainer::config::DEFAULT_ANALYZE_THRESHOLD,
                            pg_maintainer::config::DEFAULT_ANALYZE_SCALE_FACTOR,
                            e
                        ),
                    );
                    (
                        pg_maintainer::config::DEFAULT_ANALYZE_THRESHOLD,
                        pg_maintainer::config::DEFAULT_ANALYZE_SCALE_FACTOR,
                    )
                }
            };
        let effective_analyze_threshold =
            args.analyze_threshold.unwrap_or(server_analyze_threshold);
        let effective_analyze_scale_factor = args
            .analyze_scale_factor
            .unwrap_or(server_analyze_scale_factor);
        logger.log(
            LogLevel::Info,
            &format!(
                "Stale-stats thresholds: analyze_threshold={} (server: {}), analyze_scale_factor={} (server: {})",
                effective_analyze_threshold, server_analyze_threshold,
                effective_analyze_scale_factor, server_analyze_scale_factor,
            ),
        );

        logger.log(
            LogLevel::Info,
            &format!(
                "Searching for stale-stats candidates (modifications > {} + {:.2}% × live rows)...",
                effective_analyze_threshold,
                effective_analyze_scale_factor * 100.0
            ),
        );
        let candidates = operations::find_stale_stats_candidates(
            &client,
            &schemas,
            table_filter,
            effective_analyze_threshold,
            effective_analyze_scale_factor,
            min_bytes,
            max_bytes,
            limit_n,
        )
        .await
        .context("Failed to query stale-stats candidates")?;
        let summary = operations::run_stale_stats_analyze(
            &client,
            &candidates,
            effective_analyze_threshold,
            effective_analyze_scale_factor,
            args.dry_run,
            args.force,
            &already_handled,
            &logger,
            &mut shutdown_rx,
        )
        .await
        .context("ANALYZE STALE STATS phase failed")?;
        for t in &candidates {
            already_handled.insert((t.schema_name.clone(), t.table_name.clone()));
        }
        summary
    } else {
        logger.log(
            LogLevel::Info,
            "Skipping Phase 5: ANALYZE (stale stats) (not in --mode)",
        );
        Default::default()
    };

    // ── Final summary ─────────────────────────────────────────────────────────
    let elapsed = start.elapsed();
    let total_tables = vacuum_summary.total
        + analyze_summary.total
        + freeze_summary.total
        + bloat_summary.total
        + stale_stats_summary.total;
    let total_ok = vacuum_summary.succeeded
        + analyze_summary.succeeded
        + freeze_summary.succeeded
        + bloat_summary.succeeded
        + stale_stats_summary.succeeded;
    let total_fail = vacuum_summary.failed
        + analyze_summary.failed
        + freeze_summary.failed
        + bloat_summary.failed
        + stale_stats_summary.failed;

    logger.log_always(
        LogLevel::Success,
        &format!(
            "pg-maintainer completed in {:.2?} — \
             tables processed: {} | succeeded: {} | failed: {}",
            elapsed, total_tables, total_ok, total_fail
        ),
    );

    if enabled_modes.contains(&Mode::NeverVacuumed) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  VACUUM        — total: {}, ok: {}, failed: {}, skipped: {}",
                vacuum_summary.total,
                vacuum_summary.succeeded,
                vacuum_summary.failed,
                vacuum_summary.skipped
            ),
        );
    }
    if enabled_modes.contains(&Mode::NeverAnalyzed) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  ANALYZE       — total: {}, ok: {}, failed: {}, skipped: {}",
                analyze_summary.total,
                analyze_summary.succeeded,
                analyze_summary.failed,
                analyze_summary.skipped
            ),
        );
    }
    if enabled_modes.contains(&Mode::Wraparound) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  VACUUM FREEZE — total: {}, ok: {}, failed: {}, skipped: {}",
                freeze_summary.total,
                freeze_summary.succeeded,
                freeze_summary.failed,
                freeze_summary.skipped
            ),
        );
    }
    if enabled_modes.contains(&Mode::Bloated) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  VACUUM (bloat) — total: {}, ok: {}, failed: {}, skipped: {}",
                bloat_summary.total,
                bloat_summary.succeeded,
                bloat_summary.failed,
                bloat_summary.skipped
            ),
        );
    }
    if enabled_modes.contains(&Mode::StaleStats) {
        logger.log_always(
            LogLevel::Info,
            &format!(
                "  ANALYZE (stats) — total: {}, ok: {}, failed: {}, skipped: {}",
                stale_stats_summary.total,
                stale_stats_summary.succeeded,
                stale_stats_summary.failed,
                stale_stats_summary.skipped
            ),
        );
    }

    // Exit with code 130 if shutdown was requested (SIGTERM/SIGINT)
    if *shutdown_rx.borrow() {
        logger.log_always(
            LogLevel::Warning,
            "pg-maintainer was terminated by signal — exiting with code 130",
        );
        std::process::exit(130);
    }

    if total_fail > 0 {
        return Err(anyhow::anyhow!(
            "pg-maintainer completed with {} error(s)",
            total_fail
        ));
    }

    Ok(())
}
