#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedIntervalBudget {
    interval_us: u64,
    credit_us: u64,
    last_refill_us: u64,
    max_credit_us: u64,
}

impl FixedIntervalBudget {
    pub const fn new(now_us: u64, interval_us: u64, max_burst: u8) -> Self {
        let max_credit_us = interval_us.saturating_mul(max_burst as u64);
        Self {
            interval_us,
            credit_us: interval_us,
            last_refill_us: now_us,
            max_credit_us,
        }
    }

    pub fn try_take(&mut self, now_us: u64) -> bool {
        let elapsed = now_us.saturating_sub(self.last_refill_us);
        self.last_refill_us = now_us;
        self.credit_us = self
            .credit_us
            .saturating_add(elapsed)
            .min(self.max_credit_us);
        if self.credit_us < self.interval_us {
            return false;
        }
        self.credit_us -= self.interval_us;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_one_report_and_refills_at_the_fixed_interval() {
        let mut budget = FixedIntervalBudget::new(100, 1_000, 4);

        assert!(budget.try_take(100));
        assert!(!budget.try_take(1_099));
        assert!(budget.try_take(1_100));
        assert!(!budget.try_take(1_100));
    }

    #[test]
    fn idle_credit_is_limited_to_the_configured_burst() {
        let mut budget = FixedIntervalBudget::new(0, 1_000, 4);
        assert!(budget.try_take(0));

        for _ in 0..4 {
            assert!(budget.try_take(10_000));
        }
        assert!(!budget.try_take(10_000));
    }

    #[test]
    fn a_backward_clock_does_not_create_credit() {
        let mut budget = FixedIntervalBudget::new(2_000, 1_000, 1);
        assert!(budget.try_take(2_000));
        assert!(!budget.try_take(1_000));
    }
}
