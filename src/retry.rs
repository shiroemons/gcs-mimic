//! How many attempts a bounded retry gets.

/// How many attempts a bounded retry gets in all, the first one included, before its failure
/// stands. Steps that share one budget try for the same length, so a step that fails transiently
/// and one whose input keeps changing under it do not give up at different points.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RetryBudget {
    attempts: u32,
}

#[cfg_attr(not(test), expect(dead_code, reason = "the seed watcher reuses this"))]
impl RetryBudget {
    /// Three attempts: the first one and two retries.
    pub(crate) const STANDARD: Self = Self { attempts: 3 };

    /// Whether a step retried `retries` times so far may be retried once more.
    pub(crate) fn allows_retry(self, retries: u32) -> bool {
        retries.saturating_add(1) < self.attempts
    }

    /// The attempts in all, the first one included.
    pub(crate) fn attempts(self) -> u32 {
        self.attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget counts the first attempt, so it allows one retry fewer than its attempts, and a
    /// count past the end never wraps around into another retry.
    #[test]
    fn a_budget_allows_one_retry_fewer_than_its_attempts() {
        let budget = RetryBudget::STANDARD;
        assert_eq!(budget.attempts(), 3);
        assert!(budget.allows_retry(0));
        assert!(budget.allows_retry(1));
        assert!(!budget.allows_retry(2));
        assert!(!budget.allows_retry(u32::MAX));
    }
}
