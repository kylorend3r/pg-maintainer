//! Utilities for parsing and interpreting VACUUM operation results.

/// Parse the number of dead tuples removed from a before/after table snapshot.
/// Returns None if the calculation is inconclusive.
pub fn get_dead_tuples_removed(n_dead_before: i64, n_dead_after: i64) -> Option<i64> {
    let removed = n_dead_before.saturating_sub(n_dead_after);
    // Only treat it as "nothing removed" if there were dead tuples to begin with
    if removed == 0 && n_dead_before > 0 {
        Some(0)
    } else if removed > 0 {
        Some(removed)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zero_removed() {
        assert_eq!(get_dead_tuples_removed(1000, 1000), Some(0));
    }

    #[test]
    fn test_some_removed() {
        assert_eq!(get_dead_tuples_removed(1000, 500), Some(500));
    }

    #[test]
    fn test_no_dead_before() {
        assert_eq!(get_dead_tuples_removed(0, 0), None);
    }

    #[test]
    fn test_all_removed() {
        assert_eq!(get_dead_tuples_removed(100, 0), Some(100));
    }
}
