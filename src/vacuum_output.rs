//! Utilities for parsing and interpreting VACUUM operation results.

/// Estimate how much the dead-tuple count decreased across a VACUUM.
///
/// The result is **APPROXIMATE**. It is the difference between two reads of
/// `pg_stat_user_tables.n_dead_tup` (before and after the VACUUM), and that
/// column is a statistics estimate, not a count of tuples VACUUM physically
/// removed. It can be off because:
///
/// - `n_dead_tup` is itself an estimate, updated asynchronously by the
///   statistics system;
/// - concurrent writes add new dead tuples during the VACUUM, so the figure
///   understates what was cleaned;
/// - autovacuum or another session may clean the table between the two reads,
///   so work is misattributed to this run.
///
/// Returns `Some(0)` if the estimate did not decrease and there were dead
/// tuples to begin with, `Some(n)` for a decrease of `n`, and `None` if there
/// were no dead tuples before (no figure, which is not the same as "zero").
pub fn estimate_dead_tuples_reduced(n_dead_before: i64, n_dead_after: i64) -> Option<i64> {
    let reduced = n_dead_before.saturating_sub(n_dead_after);
    if reduced == 0 && n_dead_before > 0 {
        Some(0)
    } else if reduced > 0 {
        Some(reduced)
    } else {
        None
    }
}

/// Human-readable, explicitly approximate description of the dead-tuple change.
pub fn format_dead_tuple_change(op: &str, schema: &str, table: &str, reduced: i64) -> String {
    if reduced == 0 {
        format!(
            "{op} on \"{schema}\".\"{table}\": estimated dead tuples did not decrease \
             (approx., based on pg_stat_user_tables.n_dead_tup before/after; the table may not have \
             needed vacuuming, concurrent writes may have offset the reduction, or another process \
             already cleaned it up)"
        )
    } else {
        format!(
            "{op} on \"{schema}\".\"{table}\" reduced estimated dead tuples by ~{reduced} \
             (approx., from pg_stat_user_tables.n_dead_tup)"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_reduced() {
        assert_eq!(estimate_dead_tuples_reduced(1000, 1000), Some(0));
    }

    #[test]
    fn test_some_reduced() {
        assert_eq!(estimate_dead_tuples_reduced(1000, 500), Some(500));
    }

    #[test]
    fn test_no_dead_before() {
        assert_eq!(estimate_dead_tuples_reduced(0, 0), None);
    }

    #[test]
    fn test_all_reduced() {
        assert_eq!(estimate_dead_tuples_reduced(100, 0), Some(100));
    }

    #[test]
    fn test_format_zero_is_approximate() {
        let msg = format_dead_tuple_change("VACUUM", "public", "t", 0);
        assert!(msg.contains("approx"));
        assert!(msg.contains("did not decrease"));
        assert!(!msg.contains("removed"));
        assert!(msg.contains("\"public\".\"t\""));
    }

    #[test]
    fn test_format_nonzero_is_approximate() {
        let msg = format_dead_tuple_change("VACUUM", "public", "t", 42);
        assert!(msg.contains("approx"));
        assert!(msg.contains("~42"));
        assert!(!msg.contains("removed"));
    }
}
