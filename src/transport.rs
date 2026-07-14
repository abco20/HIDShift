/// Physical transport used for the currently selected output destination.
/// BLE host selection remains a separate concern inside the BLE bridge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum InputTransport {
    Ble = 1,
    EspNow = 2,
}

/// Stack-independent BLE timing policy for interactive HID links.
///
/// Keeping the policy in the core makes the latency contract testable without
/// pulling a controller or HAL into host tests. Firmware translates these
/// values to the concrete BLE stack's duration types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlePhyPreference {
    Le1M,
    Le2M,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleConnectionTiming {
    pub interval_min_us: u32,
    pub interval_max_us: u32,
    pub peripheral_latency: u16,
    pub supervision_timeout_ms: u32,
    pub preferred_phy: BlePhyPreference,
}

pub const fn low_latency_ble_connection_timing() -> BleConnectionTiming {
    BleConnectionTiming {
        // 7.5 ms is the minimum LE connection interval and gives HID reports
        // the best chance of satisfying the interactive latency target. A
        // pending peripheral notification may use the next connection event;
        // latency only permits skipping otherwise-idle events, reducing radio
        // contention with ESP-NOW without a route-switch parameter update.
        interval_min_us: 7_500,
        interval_max_us: 7_500,
        peripheral_latency: 19,
        supervision_timeout_ms: 4_000,
        preferred_phy: BlePhyPreference::Le2M,
    }
}

/// Connection-aware, single-destination transport policy.
///
/// Availability never changes the explicit selection. This avoids silently
/// redirecting keystrokes to another connected computer when the selected
/// link drops. Callers may explicitly select or cycle to another destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputTransportRouter {
    selected: InputTransport,
    ble_available: bool,
    espnow_available: bool,
}

impl InputTransportRouter {
    pub const fn new(selected: InputTransport) -> Self {
        Self {
            selected,
            ble_available: false,
            espnow_available: false,
        }
    }

    pub const fn selected(&self) -> InputTransport {
        self.selected
    }

    pub const fn active(&self) -> Option<InputTransport> {
        if self.is_available(self.selected) {
            Some(self.selected)
        } else {
            None
        }
    }

    pub const fn is_available(&self, transport: InputTransport) -> bool {
        match transport {
            InputTransport::Ble => self.ble_available,
            InputTransport::EspNow => self.espnow_available,
        }
    }

    pub fn set_available(&mut self, transport: InputTransport, available: bool) {
        match transport {
            InputTransport::Ble => self.ble_available = available,
            InputTransport::EspNow => self.espnow_available = available,
        }
    }

    pub fn select(&mut self, transport: InputTransport) {
        self.selected = transport;
    }

    pub fn select_next_available(&mut self) -> Option<InputTransport> {
        let other = match self.selected {
            InputTransport::Ble => InputTransport::EspNow,
            InputTransport::EspNow => InputTransport::Ble,
        };
        if self.is_available(other) {
            self.selected = other;
            Some(other)
        } else if self.is_available(self.selected) {
            Some(self.selected)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_connections_never_mirror_input_to_two_targets() {
        let mut router = InputTransportRouter::new(InputTransport::EspNow);
        router.set_available(InputTransport::Ble, true);
        router.set_available(InputTransport::EspNow, true);

        assert_eq!(router.active(), Some(InputTransport::EspNow));
        router.select(InputTransport::Ble);
        assert_eq!(router.active(), Some(InputTransport::Ble));
    }

    #[test]
    fn disconnecting_selected_transport_does_not_leak_input_to_the_other_pc() {
        let mut router = InputTransportRouter::new(InputTransport::EspNow);
        router.set_available(InputTransport::Ble, true);
        router.set_available(InputTransport::EspNow, true);
        router.set_available(InputTransport::EspNow, false);

        assert_eq!(router.active(), None);
        assert_eq!(router.selected(), InputTransport::EspNow);
    }

    #[test]
    fn next_available_transport_switches_only_to_a_connected_destination() {
        let mut router = InputTransportRouter::new(InputTransport::Ble);
        assert_eq!(router.select_next_available(), None);

        router.set_available(InputTransport::EspNow, true);
        assert_eq!(router.select_next_available(), Some(InputTransport::EspNow));
        assert_eq!(router.active(), Some(InputTransport::EspNow));
    }

    #[test]
    fn low_latency_ble_policy_uses_idle_event_skipping_without_relaxing_the_interval() {
        let timing = low_latency_ble_connection_timing();

        assert_eq!(timing.interval_min_us, 7_500);
        assert_eq!(timing.interval_max_us, 7_500);
        assert_eq!(timing.peripheral_latency, 19);
        assert_eq!(timing.preferred_phy, BlePhyPreference::Le2M);
        assert!(timing.supervision_timeout_ms >= 100);
        assert!(
            timing.supervision_timeout_ms * 1_000
                > 2 * u32::from(timing.peripheral_latency + 1) * timing.interval_max_us
        );
    }
}
