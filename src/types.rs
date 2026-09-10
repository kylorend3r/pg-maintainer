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
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::NeverVacuumed => write!(f, "never-vacuumed"),
            Mode::NeverAnalyzed => write!(f, "never-analyzed"),
            Mode::Wraparound => write!(f, "wraparound"),
            Mode::Bloated => write!(f, "bloated"),
            Mode::StaleStats => write!(f, "stale-stats"),
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
            _ => Err(format!(
                "Invalid mode '{s}'. Must be one of: never-vacuumed, never-analyzed, wraparound, bloated, stale-stats"
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
