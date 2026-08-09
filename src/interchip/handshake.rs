#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SpiReadyPhase {
    AwaitingReady,
    AwaitingDeassertion { transfer_started_ms: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpiReadyAction {
    Wait,
    Transfer,
    /// READY is high but its previous falling edge was not observed. The
    /// Device contract makes this safe: high means a slave DMA transaction is
    /// currently armed, regardless of whether the previous edge was missed.
    RecoverTransfer,
}

/// Prevents the SPI master from starting more than one transaction for each
/// Device DMA arm cycle.
///
/// `deassertion_observed` must come from a latched falling edge, rather than
/// only the sampled READY level. The Device may finish processing and rearm
/// between two Host samples, making the low pulse otherwise invisible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiReadyHandshake {
    phase: SpiReadyPhase,
    deassertion_timeout_ms: u64,
}

impl SpiReadyHandshake {
    pub const fn new(deassertion_timeout_ms: u64) -> Self {
        Self {
            phase: SpiReadyPhase::AwaitingReady,
            deassertion_timeout_ms,
        }
    }

    pub fn observe(
        &mut self,
        ready_high: bool,
        deassertion_observed: bool,
        now_ms: u64,
    ) -> SpiReadyAction {
        if matches!(self.phase, SpiReadyPhase::AwaitingDeassertion { .. })
            && (deassertion_observed || !ready_high)
        {
            self.phase = SpiReadyPhase::AwaitingReady;
        }

        match self.phase {
            SpiReadyPhase::AwaitingReady if ready_high => SpiReadyAction::Transfer,
            SpiReadyPhase::AwaitingDeassertion {
                transfer_started_ms,
            } if ready_high
                && now_ms.saturating_sub(transfer_started_ms) >= self.deassertion_timeout_ms =>
            {
                SpiReadyAction::RecoverTransfer
            }
            _ => SpiReadyAction::Wait,
        }
    }

    /// Commits a previously returned `Transfer` action after all local
    /// preparation has succeeded and immediately before CS can be asserted.
    pub fn mark_transfer_started(&mut self, now_ms: u64) {
        self.phase = SpiReadyPhase::AwaitingDeassertion {
            transfer_started_ms: now_ms,
        };
    }

    /// Cancels a transfer which failed before it could produce a READY edge.
    pub fn cancel_transfer(&mut self) {
        self.phase = SpiReadyPhase::AwaitingReady;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_armed_level_starts_one_transaction_only() {
        let mut handshake = SpiReadyHandshake::new(5);

        assert_eq!(handshake.observe(false, false, 10), SpiReadyAction::Wait);
        assert_eq!(handshake.observe(true, false, 10), SpiReadyAction::Transfer);
        handshake.mark_transfer_started(10);
        assert_eq!(handshake.observe(true, false, 14), SpiReadyAction::Wait);
    }

    #[test]
    fn deassertion_must_be_observed_before_the_next_transaction() {
        let mut handshake = SpiReadyHandshake::new(5);
        assert_eq!(handshake.observe(true, false, 10), SpiReadyAction::Transfer);
        handshake.mark_transfer_started(10);

        assert_eq!(handshake.observe(false, true, 11), SpiReadyAction::Wait);
        assert_eq!(handshake.observe(true, false, 12), SpiReadyAction::Transfer);
    }

    #[test]
    fn latched_low_pulse_allows_an_already_rearmed_device() {
        let mut handshake = SpiReadyHandshake::new(5);
        assert_eq!(handshake.observe(true, false, 10), SpiReadyAction::Transfer);
        handshake.mark_transfer_started(10);

        // The Device completed, drove READY low, rearmed DMA, and returned
        // READY high between two Host samples. The falling-edge latch keeps
        // that short deassertion observable.
        assert_eq!(handshake.observe(true, true, 11), SpiReadyAction::Transfer);
        handshake.mark_transfer_started(11);
        assert_eq!(handshake.observe(true, false, 12), SpiReadyAction::Wait);
    }

    #[test]
    fn a_spurious_edge_while_waiting_for_initial_ready_does_not_start() {
        let mut handshake = SpiReadyHandshake::new(5);

        assert_eq!(handshake.observe(false, true, 10), SpiReadyAction::Wait);
        assert_eq!(handshake.observe(true, false, 11), SpiReadyAction::Transfer);
    }

    #[test]
    fn preparation_cancel_does_not_consume_the_ready_cycle() {
        let mut handshake = SpiReadyHandshake::new(5);

        assert_eq!(handshake.observe(true, false, 10), SpiReadyAction::Transfer);
        assert_eq!(handshake.observe(true, false, 11), SpiReadyAction::Transfer);
        handshake.mark_transfer_started(11);
        assert_eq!(handshake.observe(true, false, 12), SpiReadyAction::Wait);
    }

    #[test]
    fn failed_transfer_can_retry_the_same_armed_device() {
        let mut handshake = SpiReadyHandshake::new(5);
        assert_eq!(handshake.observe(true, false, 10), SpiReadyAction::Transfer);
        handshake.mark_transfer_started(10);

        handshake.cancel_transfer();

        assert_eq!(handshake.observe(true, false, 11), SpiReadyAction::Transfer);
    }

    #[test]
    fn armed_device_recovers_when_a_short_deassertion_edge_is_missed() {
        let mut handshake = SpiReadyHandshake::new(5);
        assert_eq!(handshake.observe(true, false, 10), SpiReadyAction::Transfer);
        handshake.mark_transfer_started(10);

        assert_eq!(handshake.observe(true, false, 14), SpiReadyAction::Wait);
        assert_eq!(
            handshake.observe(true, false, 15),
            SpiReadyAction::RecoverTransfer
        );
        handshake.mark_transfer_started(15);
        assert_eq!(handshake.observe(true, false, 19), SpiReadyAction::Wait);
    }
}
