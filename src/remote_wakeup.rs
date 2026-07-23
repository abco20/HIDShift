pub const REMOTE_WAKEUP_SIGNAL_MS: u64 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RemoteWakeupAction {
    AssertSignal,
    ClearSignal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RemoteWakeupController {
    signal_started_ms: Option<u64>,
    attempted_in_suspend: bool,
}

impl RemoteWakeupController {
    pub const fn new() -> Self {
        Self {
            signal_started_ms: None,
            attempted_in_suspend: false,
        }
    }

    pub fn on_activity(
        &mut self,
        now_ms: u64,
        suspended: bool,
        host_enabled: bool,
    ) -> Option<RemoteWakeupAction> {
        if !suspended
            || !host_enabled
            || self.attempted_in_suspend
            || self.signal_started_ms.is_some()
        {
            return None;
        }

        self.attempted_in_suspend = true;
        self.signal_started_ms = Some(now_ms);
        Some(RemoteWakeupAction::AssertSignal)
    }

    pub fn poll(&mut self, now_ms: u64, suspended: bool) -> Option<RemoteWakeupAction> {
        if self
            .signal_started_ms
            .is_some_and(|started| now_ms.saturating_sub(started) >= REMOTE_WAKEUP_SIGNAL_MS)
        {
            self.signal_started_ms = None;
            return Some(RemoteWakeupAction::ClearSignal);
        }

        if !suspended && self.signal_started_ms.is_none() {
            self.attempted_in_suspend = false;
        }
        None
    }

    pub const fn signal_asserted(&self) -> bool {
        self.signal_started_ms.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_asserts_only_while_suspended_and_enabled_by_host() {
        for (suspended, host_enabled) in [(false, false), (false, true), (true, false)] {
            let mut wakeup = RemoteWakeupController::new();
            assert_eq!(wakeup.on_activity(100, suspended, host_enabled), None);
            assert!(!wakeup.signal_asserted());
        }

        let mut wakeup = RemoteWakeupController::new();
        assert_eq!(
            wakeup.on_activity(100, true, true),
            Some(RemoteWakeupAction::AssertSignal)
        );
        assert!(wakeup.signal_asserted());
    }

    #[test]
    fn signal_is_cleared_after_ten_milliseconds_without_blocking() {
        let mut wakeup = RemoteWakeupController::new();
        assert_eq!(
            wakeup.on_activity(50, true, true),
            Some(RemoteWakeupAction::AssertSignal)
        );
        assert_eq!(wakeup.poll(59, true), None);
        assert!(wakeup.signal_asserted());
        assert_eq!(wakeup.poll(60, true), Some(RemoteWakeupAction::ClearSignal));
        assert!(!wakeup.signal_asserted());
    }

    #[test]
    fn only_one_wakeup_attempt_is_made_per_suspend_period() {
        let mut wakeup = RemoteWakeupController::new();
        assert_eq!(
            wakeup.on_activity(0, true, true),
            Some(RemoteWakeupAction::AssertSignal)
        );
        assert_eq!(wakeup.on_activity(1, true, true), None);
        assert_eq!(
            wakeup.poll(REMOTE_WAKEUP_SIGNAL_MS, true),
            Some(RemoteWakeupAction::ClearSignal)
        );
        assert_eq!(wakeup.on_activity(20, true, true), None);

        assert_eq!(wakeup.poll(21, false), None);
        assert_eq!(wakeup.poll(22, true), None);
        assert_eq!(
            wakeup.on_activity(23, true, true),
            Some(RemoteWakeupAction::AssertSignal)
        );
    }

    #[test]
    fn signal_is_still_cleared_if_host_resumes_during_pulse() {
        let mut wakeup = RemoteWakeupController::new();
        assert_eq!(
            wakeup.on_activity(100, true, true),
            Some(RemoteWakeupAction::AssertSignal)
        );
        assert_eq!(wakeup.poll(105, false), None);
        assert_eq!(
            wakeup.poll(110, false),
            Some(RemoteWakeupAction::ClearSignal)
        );
        assert!(!wakeup.signal_asserted());
    }
}
