/// Emits one safety release when the realtime link disappears.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkWatchdog {
    timeout_ms: u64,
    last_received_ms: Option<u64>,
    release_emitted: bool,
}

impl LinkWatchdog {
    pub const fn new(timeout_ms: u64) -> Self {
        Self {
            timeout_ms,
            last_received_ms: None,
            release_emitted: false,
        }
    }

    pub fn observe_packet(&mut self, now_ms: u64) {
        self.last_received_ms = Some(now_ms);
        self.release_emitted = false;
    }

    pub fn take_release_required(&mut self, now_ms: u64) -> bool {
        let timed_out = self
            .last_received_ms
            .is_some_and(|last| now_ms.wrapping_sub(last) >= self.timeout_ms);
        if timed_out && !self.release_emitted {
            self.release_emitted = true;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_loss_emits_one_release_until_a_packet_reconnects() {
        let mut watchdog = LinkWatchdog::new(20);
        watchdog.observe_packet(100);
        assert!(!watchdog.take_release_required(119));
        assert!(watchdog.take_release_required(120));
        assert!(!watchdog.take_release_required(121));
        watchdog.observe_packet(130);
        assert!(watchdog.take_release_required(150));
    }
}
