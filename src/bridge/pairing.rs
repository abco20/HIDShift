use crate::ids::HostId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingMode {
    current: Option<PairingSession>,
}

impl PairingMode {
    pub const fn new() -> Self {
        Self { current: None }
    }

    pub const fn current(&self) -> Option<PairingSession> {
        self.current
    }

    pub fn enter(&mut self, host_id: HostId, now_ms: u64, duration_ms: u64) {
        self.current = Some(PairingSession {
            host_id,
            expires_at_ms: now_ms.saturating_add(duration_ms),
        });
    }

    pub fn cancel(&mut self) {
        self.current = None;
    }

    pub fn clear_host(&mut self, host_id: HostId) {
        if self.is_open_for(host_id) {
            self.current = None;
        }
    }

    pub fn tick(&mut self, now_ms: u64) -> Option<HostId> {
        let session = self.current?;
        if now_ms < session.expires_at_ms {
            return None;
        }
        self.current = None;
        Some(session.host_id)
    }

    pub fn is_open_for(&self, host_id: HostId) -> bool {
        matches!(self.current, Some(session) if session.host_id == host_id)
    }
}

impl Default for PairingMode {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingSession {
    pub host_id: HostId,
    pub expires_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_mode_tracks_one_slot_with_expiry() {
        let mut pairing = PairingMode::new();
        pairing.enter(HostId(2), 100, 3_000);

        assert!(pairing.is_open_for(HostId(2)));
        assert_eq!(pairing.tick(3_099), None);
        assert_eq!(pairing.tick(3_100), Some(HostId(2)));
        assert!(!pairing.is_open_for(HostId(2)));
    }

    #[test]
    fn entering_new_slot_replaces_old_pairing_window() {
        let mut pairing = PairingMode::new();
        pairing.enter(HostId(1), 0, 1_000);
        pairing.enter(HostId(3), 10, 2_000);

        assert!(!pairing.is_open_for(HostId(1)));
        assert!(pairing.is_open_for(HostId(3)));
    }

    #[test]
    fn clearing_active_slot_cancels_pairing_window() {
        let mut pairing = PairingMode::new();
        pairing.enter(HostId(1), 50, 500);
        pairing.clear_host(HostId(1));

        assert_eq!(pairing.current(), None);
    }
}
