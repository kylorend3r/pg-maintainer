// Connection defaults
pub const DEFAULT_POSTGRES_HOST: &str = "localhost";
pub const DEFAULT_POSTGRES_PORT: u16 = 5432;
pub const DEFAULT_POSTGRES_DATABASE: &str = "postgres";
pub const DEFAULT_POSTGRES_USERNAME: &str = "postgres";

// Maintenance work memory
pub const DEFAULT_MAINTENANCE_WORK_MEM_GB: u64 = 1;
pub const MAX_MAINTENANCE_WORK_MEM_GB: u64 = 32;

// XID age threshold: tables whose age exceeds autovacuum_freeze_max_age are wraparound candidates.
// This default aligns with PostgreSQL's built-in autovacuum trigger point.
// Can be overridden per-run via --wraparound-min-age.
pub const DEFAULT_WRAPAROUND_MIN_AGE: i64 = 200_000_000;

// Bloat detection thresholds
pub const DEFAULT_BLOAT_THRESHOLD_PCT: f64 = 80.0;
pub const DEFAULT_BLOAT_MIN_DEAD_TUP: i64 = 1000;
