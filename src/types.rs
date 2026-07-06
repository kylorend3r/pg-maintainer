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
                "Invalid sslmode '{}'. Must be one of: disable, require, verify-ca, verify-full",
                s
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
                "Invalid log format '{}'. Must be one of: 'text', 'json'",
                s
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
    Vacuum,
    Analyze,
    Freeze,
    Bloat,
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Vacuum => write!(f, "vacuum"),
            Mode::Analyze => write!(f, "analyze"),
            Mode::Freeze => write!(f, "freeze"),
            Mode::Bloat => write!(f, "bloat"),
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "vacuum" => Ok(Mode::Vacuum),
            "analyze" => Ok(Mode::Analyze),
            "freeze" => Ok(Mode::Freeze),
            "bloat" => Ok(Mode::Bloat),
            _ => Err(format!(
                "Invalid mode '{}'. Must be one of: vacuum, analyze, freeze, bloat",
                s
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
