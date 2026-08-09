#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryPhase {
    Polling { last_progress_ms: u64 },
    Quiet { until_ms: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpiLinkRecoveryAction {
    Poll,
    EnterQuiet,
    StayQuiet,
    ResumePolling,
}

/// Forces a receive-only peer to observe silence long enough to reset a
/// wedged transmit path before polling resumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiLinkRecovery {
    phase: RecoveryPhase,
    progress_timeout_ms: u64,
    quiet_period_ms: u64,
}

/// Detects transactions that complete without any decodable peer cell. A
/// completely absent master never arms this watchdog, so standalone fallback
/// USB operation remains stable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiTransactionWatchdog {
    last_valid_or_first_transaction_ms: Option<u64>,
    timeout_ms: u64,
}

impl SpiTransactionWatchdog {
    pub const fn new(timeout_ms: u64) -> Self {
        Self {
            last_valid_or_first_transaction_ms: None,
            timeout_ms,
        }
    }

    pub fn observe_transaction(&mut self, now_ms: u64) {
        if self.last_valid_or_first_transaction_ms.is_none() {
            self.last_valid_or_first_transaction_ms = Some(now_ms);
        }
    }

    pub fn observe_valid_cell(&mut self, now_ms: u64) {
        self.last_valid_or_first_transaction_ms = Some(now_ms);
    }

    pub fn timed_out(self, now_ms: u64) -> bool {
        self.last_valid_or_first_transaction_ms
            .is_some_and(|last| now_ms.saturating_sub(last) >= self.timeout_ms)
    }
}

impl SpiLinkRecovery {
    pub const fn new(started_ms: u64, progress_timeout_ms: u64, quiet_period_ms: u64) -> Self {
        Self {
            phase: RecoveryPhase::Polling {
                last_progress_ms: started_ms,
            },
            progress_timeout_ms,
            quiet_period_ms,
        }
    }

    pub fn observe_valid_cell(&mut self, now_ms: u64) {
        if let RecoveryPhase::Polling { last_progress_ms } = &mut self.phase {
            *last_progress_ms = now_ms;
        }
    }

    pub fn advance(&mut self, now_ms: u64) -> SpiLinkRecoveryAction {
        match self.phase {
            RecoveryPhase::Polling { last_progress_ms }
                if now_ms.saturating_sub(last_progress_ms) >= self.progress_timeout_ms =>
            {
                self.phase = RecoveryPhase::Quiet {
                    until_ms: now_ms.saturating_add(self.quiet_period_ms),
                };
                SpiLinkRecoveryAction::EnterQuiet
            }
            RecoveryPhase::Polling { .. } => SpiLinkRecoveryAction::Poll,
            RecoveryPhase::Quiet { until_ms } if now_ms < until_ms => {
                SpiLinkRecoveryAction::StayQuiet
            }
            RecoveryPhase::Quiet { .. } => {
                self.phase = RecoveryPhase::Polling {
                    last_progress_ms: now_ms,
                };
                SpiLinkRecoveryAction::ResumePolling
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_startup_response_forces_a_quiet_recovery_window() {
        let mut recovery = SpiLinkRecovery::new(100, 1_500, 2_000);

        assert_eq!(recovery.advance(1_599), SpiLinkRecoveryAction::Poll);
        assert_eq!(recovery.advance(1_600), SpiLinkRecoveryAction::EnterQuiet);
        assert_eq!(recovery.advance(3_599), SpiLinkRecoveryAction::StayQuiet);
        assert_eq!(
            recovery.advance(3_600),
            SpiLinkRecoveryAction::ResumePolling
        );
        assert_eq!(recovery.advance(3_601), SpiLinkRecoveryAction::Poll);
    }

    #[test]
    fn valid_cells_extend_the_polling_window() {
        let mut recovery = SpiLinkRecovery::new(0, 1_500, 2_000);
        recovery.observe_valid_cell(1_400);

        assert_eq!(recovery.advance(2_899), SpiLinkRecoveryAction::Poll);
        assert_eq!(recovery.advance(2_900), SpiLinkRecoveryAction::EnterQuiet);
    }

    #[test]
    fn failed_recovery_attempt_can_start_another_quiet_window() {
        let mut recovery = SpiLinkRecovery::new(0, 10, 20);

        assert_eq!(recovery.advance(10), SpiLinkRecoveryAction::EnterQuiet);
        assert_eq!(recovery.advance(30), SpiLinkRecoveryAction::ResumePolling);
        assert_eq!(recovery.advance(40), SpiLinkRecoveryAction::EnterQuiet);
    }

    #[test]
    fn transaction_watchdog_is_not_armed_without_a_master() {
        let watchdog = SpiTransactionWatchdog::new(1_500);

        assert!(!watchdog.timed_out(u64::MAX));
    }

    #[test]
    fn malformed_transactions_eventually_time_out() {
        let mut watchdog = SpiTransactionWatchdog::new(1_500);
        watchdog.observe_transaction(10_000);

        assert!(!watchdog.timed_out(11_499));
        assert!(watchdog.timed_out(11_500));
    }

    #[test]
    fn valid_cells_keep_transaction_watchdog_alive() {
        let mut watchdog = SpiTransactionWatchdog::new(1_500);
        watchdog.observe_transaction(100);
        watchdog.observe_valid_cell(1_500);

        assert!(!watchdog.timed_out(2_999));
        assert!(watchdog.timed_out(3_000));
    }
}
