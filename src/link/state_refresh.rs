/// Quiet period before the one bounded critical-state refresh. A newer input
/// supersedes this schedule and already carries the critical history, while an
/// idle lost release is still recovered inside the 15 ms radio latency budget.
/// Four milliseconds leaves enough time for the refresh to clear the radio
/// queue before the next input in the common 7.5 ms polling cadence.
pub const CRITICAL_STATE_REFRESH_INTERVAL_US: u64 = 4_000;

/// Number of state refreshes after the primary broadcast. These are current
/// state snapshots, not acknowledgement-driven packet retransmissions. A
/// newer critical state supersedes the whole schedule.
pub const CRITICAL_STATE_REFRESH_COUNT: u8 = 1;

/// Allocation-free schedule for bounded current-state refreshes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CriticalStateRefresh {
    remaining: u8,
    next_due_us: u64,
}

impl CriticalStateRefresh {
    pub const fn after_primary(now_us: u64) -> Self {
        Self {
            remaining: CRITICAL_STATE_REFRESH_COUNT,
            next_due_us: now_us.saturating_add(CRITICAL_STATE_REFRESH_INTERVAL_US),
        }
    }

    /// A motion snapshot already carries the current critical state, so begin
    /// the quiet-period timer again instead of sending a competing snapshot.
    pub fn defer_after_piggyback(&mut self, now_us: u64) {
        self.next_due_us = now_us.saturating_add(CRITICAL_STATE_REFRESH_INTERVAL_US);
    }

    pub fn take_due(&mut self, now_us: u64) -> bool {
        if self.remaining == 0 || now_us < self.next_due_us {
            return false;
        }
        self.remaining -= 1;
        self.next_due_us = now_us.saturating_add(CRITICAL_STATE_REFRESH_INTERVAL_US);
        true
    }

    pub const fn is_complete(self) -> bool {
        self.remaining == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_state_refreshes_fit_inside_the_tail_latency_budget() {
        let mut refresh = CriticalStateRefresh::after_primary(10_000);

        assert!(!refresh.take_due(13_999));
        assert!(refresh.take_due(14_000));
        assert!(refresh.is_complete());
        assert!(!refresh.take_due(18_000));
        assert!(CRITICAL_STATE_REFRESH_INTERVAL_US < 15_000);
    }

    #[test]
    fn motion_piggyback_defers_a_separate_state_refresh() {
        let mut refresh = CriticalStateRefresh::after_primary(10_000);
        refresh.defer_after_piggyback(13_000);

        assert!(!refresh.take_due(16_999));
        assert!(refresh.take_due(17_000));
    }
}
