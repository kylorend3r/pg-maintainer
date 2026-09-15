/// VACUUM operation options.
#[derive(Debug, Clone, Copy)]
pub struct VacuumOptions {
    pub truncate: bool,
    pub disable_page_skipping: bool,
    pub skip_locked: bool,
}

/// Shared run-level policy flags threaded through every maintenance phase.
#[derive(Debug, Clone, Copy)]
pub struct RunPolicy {
    pub dry_run: bool,
    pub force: bool,
    pub skip_active_vacuum: bool,
}

/// Session-scoped I/O throttling settings (`vacuum_cost_delay`/`vacuum_cost_limit`).
///
/// `None` means "leave the server's value alone". A `Some(0.0)` delay is an
/// explicit *disable* — it still issues the `SET`, overriding whatever
/// `postgresql.conf` configured.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ThrottleSettings {
    pub cost_delay_ms: Option<f64>,
    pub cost_limit: Option<i32>,
}

impl ThrottleSettings {
    /// Combine the explicit flags with the `--gentle` preset.
    ///
    /// An explicitly-given value always wins; `--gentle` only fills in whichever
    /// of the two was left unset.
    pub fn resolve(cost_delay_ms: Option<f64>, cost_limit: Option<i32>, gentle: bool) -> Self {
        if gentle {
            Self {
                cost_delay_ms: cost_delay_ms.or(Some(crate::config::GENTLE_VACUUM_COST_DELAY_MS)),
                cost_limit: cost_limit.or(Some(crate::config::GENTLE_VACUUM_COST_LIMIT)),
            }
        } else {
            Self {
                cost_delay_ms,
                cost_limit,
            }
        }
    }

    /// Check both values against PostgreSQL's accepted GUC ranges.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(delay) = self.cost_delay_ms
            && !(0.0..=crate::config::MAX_VACUUM_COST_DELAY_MS).contains(&delay)
        {
            return Err(format!(
                "--vacuum-cost-delay-ms ({delay}) must be between 0 and {}",
                crate::config::MAX_VACUUM_COST_DELAY_MS
            ));
        }
        if let Some(limit) = self.cost_limit
            && !(crate::config::MIN_VACUUM_COST_LIMIT..=crate::config::MAX_VACUUM_COST_LIMIT)
                .contains(&limit)
        {
            return Err(format!(
                "--vacuum-cost-limit ({limit}) must be between {} and {}",
                crate::config::MIN_VACUUM_COST_LIMIT,
                crate::config::MAX_VACUUM_COST_LIMIT
            ));
        }
        Ok(())
    }

    /// True when at least one `SET` will be issued for this session.
    pub fn is_enabled(&self) -> bool {
        self.cost_delay_ms.is_some() || self.cost_limit.is_some()
    }

    /// True when a non-zero delay will actually throttle the session.
    ///
    /// Distinct from [`Self::is_enabled`]: `--vacuum-cost-delay-ms 0` is an
    /// explicit disable, so it is enabled but not active.
    pub fn is_active(&self) -> bool {
        self.cost_delay_ms.is_some_and(|d| d > 0.0)
    }
}

/// A table identified by schema + name with optional row-count hints from pg_stat_user_tables.
#[derive(Debug, Clone)]
pub struct TableInfo {
    pub schema_name: String,
    pub table_name: String,
    /// Estimated live row count from pg_stat_user_tables (may be -1 if not available)
    pub n_live_tup: i64,
    /// Estimated dead row count — useful for ordering vacuum candidates
    pub n_dead_tup: i64,
}

/// A table that is a candidate for anti-wraparound freezing.
#[derive(Debug, Clone)]
pub struct FreezeTableInfo {
    pub schema_name: String,
    pub table_name: String,
    /// Current transaction age of relfrozenxid
    pub xid_age: i64,
    /// The autovacuum_freeze_max_age threshold read from the server at query time
    pub freeze_max_age: i64,
}

impl FreezeTableInfo {
    /// Percentage of the freeze window consumed (0–100+)
    pub fn pct_toward_wraparound(&self) -> f64 {
        if self.freeze_max_age == 0 {
            return 100.0;
        }
        (self.xid_age as f64 / self.freeze_max_age as f64) * 100.0
    }
}

/// SSL connection mode, matching libpq sslmode semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SslMode {
    #[default]
    Disable,
    Require,
    VerifyCa,
    VerifyFull,
}

impl std::fmt::Display for SslMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SslMode::Disable => write!(f, "disable"),
            SslMode::Require => write!(f, "require"),
            SslMode::VerifyCa => write!(f, "verify-ca"),
            SslMode::VerifyFull => write!(f, "verify-full"),
        }
    }
}

impl std::str::FromStr for SslMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "disable" => Ok(SslMode::Disable),
            "require" => Ok(SslMode::Require),
            "verify-ca" => Ok(SslMode::VerifyCa),
            "verify-full" => Ok(SslMode::VerifyFull),
            _ => Err(format!(
                "Invalid sslmode '{s}'. Must be one of: disable, require, verify-ca, verify-full"
            )),
        }
    }
}

/// Log output format (text or JSON)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogFormat {
    #[default]
    Text,
    Json,
}

impl std::fmt::Display for LogFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogFormat::Text => write!(f, "text"),
            LogFormat::Json => write!(f, "json"),
        }
    }
}

impl std::str::FromStr for LogFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "text" => Ok(LogFormat::Text),
            "json" => Ok(LogFormat::Json),
            _ => Err(format!(
                "Invalid log format '{s}'. Must be one of: 'text', 'json'"
            )),
        }
    }
}

/// Per-operation result counters
#[derive(Debug, Default)]
pub struct OperationSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Maintenance mode: which phase(s) to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    NeverVacuumed,
    NeverAnalyzed,
    Wraparound,
    Bloated,
    StaleStats,
    VacuumOverdue,
    AnalyzeOverdue,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::NeverVacuumed => write!(f, "never-vacuumed"),
            Mode::NeverAnalyzed => write!(f, "never-analyzed"),
            Mode::Wraparound => write!(f, "wraparound"),
            Mode::Bloated => write!(f, "bloated"),
            Mode::StaleStats => write!(f, "stale-stats"),
            Mode::VacuumOverdue => write!(f, "vacuum-overdue"),
            Mode::AnalyzeOverdue => write!(f, "analyze-overdue"),
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "never-vacuumed" => Ok(Mode::NeverVacuumed),
            "never-analyzed" => Ok(Mode::NeverAnalyzed),
            "wraparound" => Ok(Mode::Wraparound),
            "bloated" => Ok(Mode::Bloated),
            "stale-stats" => Ok(Mode::StaleStats),
            "vacuum-overdue" => Ok(Mode::VacuumOverdue),
            "analyze-overdue" => Ok(Mode::AnalyzeOverdue),
            _ => Err(format!(
                "Invalid mode '{s}'. Must be one of: never-vacuumed, never-analyzed, wraparound, bloated, stale-stats, vacuum-overdue, analyze-overdue"
            )),
        }
    }
}

/// A table that is a candidate for bloat cleanup (excessive dead tuples).
#[derive(Debug, Clone)]
pub struct BloatTableInfo {
    pub schema_name: String,
    pub table_name: String,
    /// Estimated live row count from pg_stat_user_tables
    pub n_live_tup: i64,
    /// Estimated dead row count — used to compute bloat percentage
    pub n_dead_tup: i64,
}

impl BloatTableInfo {
    /// Percentage of tuples that are dead (0–100+)
    pub fn pct_bloat(&self) -> f64 {
        let total = self.n_live_tup + self.n_dead_tup;
        if total == 0 {
            return 0.0;
        }
        (self.n_dead_tup as f64 / total as f64) * 100.0
    }
}

/// A table that is a candidate for re-analysis because enough rows have changed
/// since the last analyze that planner statistics are likely stale.
#[derive(Debug, Clone)]
pub struct StaleStatsTableInfo {
    pub schema_name: String,
    pub table_name: String,
    /// Estimated live row count from pg_stat_user_tables
    pub n_live_tup: i64,
    /// Rows inserted/updated/deleted since the last ANALYZE (manual or auto)
    pub n_mod_since_analyze: i64,
}

impl StaleStatsTableInfo {
    /// The absolute modification-count threshold that was crossed, given the
    /// configured flat floor and scale factor.
    pub fn effective_threshold(&self, analyze_threshold: i64, analyze_scale_factor: f64) -> i64 {
        analyze_threshold + (analyze_scale_factor * self.n_live_tup as f64).round() as i64
    }
}

/// A table whose most recent VACUUM (manual or auto) is older than the configured
/// number of days.
#[derive(Debug, Clone)]
pub struct OverdueVacuumTableInfo {
    pub schema_name: String,
    pub table_name: String,
    pub n_live_tup: i64,
    pub n_dead_tup: i64,
    /// Days since GREATEST(last_vacuum, last_autovacuum), fractional.
    pub days_since_vacuum: f64,
}

/// A table whose most recent ANALYZE (manual or auto) is older than the configured
/// number of days.
#[derive(Debug, Clone)]
pub struct OverdueAnalyzeTableInfo {
    pub schema_name: String,
    pub table_name: String,
    pub n_live_tup: i64,
    pub n_mod_since_analyze: i64,
    /// Days since GREATEST(last_analyze, last_autoanalyze), fractional.
    pub days_since_analyze: f64,
}

/// A table named explicitly on `--also-tables`, always schema-qualified.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExplicitTable {
    pub schema_name: String,
    pub table_name: String,
}

impl ExplicitTable {
    /// Parse `schema.table` entries, preserving the order given and collapsing
    /// duplicates.
    ///
    /// Qualification is mandatory: a bare table name is rejected rather than
    /// resolved against every schema, so an explicit list can never touch a
    /// same-named table in a schema the operator forgot about. Identifiers
    /// containing a literal `.` are not supported here.
    pub fn parse_list(entries: &[String]) -> Result<Vec<ExplicitTable>, String> {
        let mut out: Vec<ExplicitTable> = Vec::new();
        for raw in entries {
            let entry = raw.trim();
            if entry.is_empty() {
                continue;
            }
            let Some((schema, table)) = entry.split_once('.') else {
                return Err(format!(
                    "--also-tables entry '{entry}' must be schema-qualified, e.g. public.{entry}"
                ));
            };
            if schema.is_empty() || table.is_empty() {
                return Err(format!(
                    "--also-tables entry '{entry}' is not a valid schema.table name"
                ));
            }
            if table.contains('.') {
                return Err(format!(
                    "--also-tables entry '{entry}' has more than one '.' — expected schema.table"
                ));
            }
            let candidate = ExplicitTable {
                schema_name: schema.to_string(),
                table_name: table.to_string(),
            };
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
        Ok(out)
    }
}

/// One row of `pg_stat_replication`, reduced to what the lag gate needs.
#[derive(Debug, Clone, PartialEq)]
pub struct StandbyLag {
    pub application_name: String,
    pub state: String,
    pub sync_state: String,
    /// `None` means the standby is caught up and there is no recent WAL to
    /// measure against — not that the lag is unknown.
    pub replay_lag_seconds: Option<f64>,
}

/// What a read of `pg_stat_replication` told us.
///
/// `NoReplicas` and `Unobservable` both come back as an empty view and must not
/// be conflated: the first is a healthy single-primary instance, the second is a
/// role without `pg_read_all_stats` that simply cannot see the rows.
#[derive(Debug, Clone, PartialEq)]
pub enum LagObservation {
    NoReplicas,
    Unobservable,
    Observed(Vec<StandbyLag>),
}

impl LagObservation {
    /// Worst replay lag across every standby, treating a caught-up `NULL` as zero.
    /// `None` only when lag could not be observed at all.
    pub fn max_lag_seconds(&self) -> Option<f64> {
        match self {
            LagObservation::NoReplicas => Some(0.0),
            LagObservation::Unobservable => None,
            LagObservation::Observed(standbys) => Some(
                standbys
                    .iter()
                    .map(|s| s.replay_lag_seconds.unwrap_or(0.0))
                    .fold(0.0_f64, f64::max),
            ),
        }
    }

    /// True only when lag is both observable and over the threshold.
    pub fn exceeds(&self, threshold_seconds: f64) -> bool {
        match self.max_lag_seconds() {
            Some(max) => max > threshold_seconds,
            None => false,
        }
    }

    /// The standby responsible for `max_lag_seconds`, for logging.
    pub fn worst_standby(&self) -> Option<&StandbyLag> {
        match self {
            LagObservation::Observed(standbys) => standbys.iter().max_by(|a, b| {
                a.replay_lag_seconds
                    .unwrap_or(0.0)
                    .total_cmp(&b.replay_lag_seconds.unwrap_or(0.0))
            }),
            _ => None,
        }
    }

    /// Number of standbys seen.
    pub fn standby_count(&self) -> usize {
        match self {
            LagObservation::Observed(standbys) => standbys.len(),
            _ => 0,
        }
    }
}

/// Verdict from the per-table replication-lag gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LagGateVerdict {
    Proceed,
    SkipTable,
    ShutdownRequested,
}

/// Per-table replication-lag gate: thresholds plus the run-scoped state it
/// accumulates.
///
/// State lives behind atomics so the gate can be shared by `&` across every
/// runner, the same way `Arc<Logger>` already is, instead of threading `&mut`
/// through six call sites.
#[derive(Debug)]
pub struct ReplicaLagGate {
    pub threshold_seconds: f64,
    pub max_wait_seconds: u64,
    pub poll_interval_seconds: u64,
    disabled: std::sync::atomic::AtomicBool,
    total_waited_ms: std::sync::atomic::AtomicU64,
    tables_skipped: std::sync::atomic::AtomicUsize,
}

impl ReplicaLagGate {
    pub fn new(threshold_seconds: f64, max_wait_seconds: u64) -> Self {
        Self {
            threshold_seconds,
            max_wait_seconds,
            poll_interval_seconds: crate::config::REPLICA_LAG_POLL_INTERVAL_SECONDS,
            disabled: std::sync::atomic::AtomicBool::new(false),
            total_waited_ms: std::sync::atomic::AtomicU64::new(0),
            tables_skipped: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Turned off for the rest of the run once we learn there is nothing to watch.
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn disable(&self) {
        self.disabled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn add_waited(&self, elapsed: std::time::Duration) {
        self.total_waited_ms.fetch_add(
            elapsed.as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    pub fn record_skip(&self) {
        self.tables_skipped
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn total_waited(&self) -> std::time::Duration {
        std::time::Duration::from_millis(
            self.total_waited_ms
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    pub fn tables_skipped(&self) -> usize {
        self.tables_skipped
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}
