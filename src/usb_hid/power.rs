pub const USB_HOST_SUSPEND_DELAY_MS: u64 = 3_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHostBusState {
    Running,
    Suspended,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbHostPowerPolicy {
    state: UsbHostBusState,
    suspend_deadline_ms: Option<u64>,
}

impl UsbHostPowerPolicy {
    pub const fn new() -> Self {
        Self {
            state: UsbHostBusState::Running,
            suspend_deadline_ms: None,
        }
    }

    pub const fn state(self) -> UsbHostBusState {
        self.state
    }

    pub fn update(
        &mut self,
        now_ms: u64,
        usb_device_present: bool,
        computer_connected: bool,
    ) -> Option<UsbHostBusState> {
        let should_run = !usb_device_present || computer_connected;
        if should_run {
            self.suspend_deadline_ms = None;
            if self.state == UsbHostBusState::Suspended {
                self.state = UsbHostBusState::Running;
                return Some(self.state);
            }
            return None;
        }

        if self.state == UsbHostBusState::Suspended {
            return None;
        }

        let deadline = self
            .suspend_deadline_ms
            .get_or_insert_with(|| now_ms.saturating_add(USB_HOST_SUSPEND_DELAY_MS));
        if now_ms < *deadline {
            return None;
        }

        self.suspend_deadline_ms = None;
        self.state = UsbHostBusState::Suspended;
        Some(self.state)
    }
}

impl Default for UsbHostPowerPolicy {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspends_only_after_an_unconnected_usb_device_stays_idle_for_the_delay() {
        let mut policy = UsbHostPowerPolicy::new();

        assert_eq!(policy.update(10, true, false), None);
        assert_eq!(policy.update(3_009, true, false), None);
        assert_eq!(
            policy.update(3_010, true, false),
            Some(UsbHostBusState::Suspended)
        );
        assert_eq!(policy.state(), UsbHostBusState::Suspended);
        assert_eq!(policy.update(8_000, true, false), None);
    }

    #[test]
    fn a_computer_connection_cancels_pending_suspend_and_resumes_immediately() {
        let mut policy = UsbHostPowerPolicy::new();

        assert_eq!(policy.update(100, true, false), None);
        assert_eq!(policy.update(2_000, true, true), None);
        assert_eq!(policy.update(6_000, true, true), None);

        assert_eq!(policy.update(7_000, true, false), None);
        assert_eq!(
            policy.update(10_000, true, false),
            Some(UsbHostBusState::Suspended)
        );
        assert_eq!(
            policy.update(10_001, true, true),
            Some(UsbHostBusState::Running)
        );
    }

    #[test]
    fn removing_the_last_usb_device_resumes_the_bus_for_future_enumeration() {
        let mut policy = UsbHostPowerPolicy::new();

        policy.update(0, true, false);
        assert_eq!(
            policy.update(USB_HOST_SUSPEND_DELAY_MS, true, false),
            Some(UsbHostBusState::Suspended)
        );
        assert_eq!(
            policy.update(USB_HOST_SUSPEND_DELAY_MS + 1, false, false),
            Some(UsbHostBusState::Running)
        );
        assert_eq!(policy.update(9_000, false, false), None);
    }
}
