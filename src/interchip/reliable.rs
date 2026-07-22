use super::cell::{SPI_CELL_PAYLOAD_LEN, SpiCell};

pub const SPI_TX_WINDOW: usize = 4;

/// Holds one application command across link-session renegotiation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReliableCommandSlot<T> {
    value: Option<T>,
    queued: bool,
}

impl<T: Copy> ReliableCommandSlot<T> {
    pub const fn new() -> Self {
        Self {
            value: None,
            queued: false,
        }
    }

    pub fn stage(&mut self, value: T) {
        self.value = Some(value);
        self.queued = false;
    }

    pub const fn value(self) -> Option<T> {
        self.value
    }

    pub fn mark_queued(&mut self) {
        self.queued = self.value.is_some();
    }

    pub fn sender_reset(&mut self) {
        self.queued = false;
    }

    pub fn acknowledge(&mut self, command_completed: bool) -> Option<T> {
        if !self.queued {
            return None;
        }
        self.queued = false;
        if command_completed {
            self.value.take()
        } else {
            None
        }
    }

    pub fn complete(&mut self) -> Option<T> {
        self.queued = false;
        self.value.take()
    }
}

impl<T: Copy> Default for ReliableCommandSlot<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingCell {
    cell: SpiCell,
    last_sent_ms: u64,
    attempts: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReliableSender {
    session_id: u32,
    next_sequence: u16,
    cumulative_ack: u16,
    pending: [Option<PendingCell>; SPI_TX_WINDOW],
}

impl ReliableSender {
    pub const fn new(session_id: u32) -> Self {
        Self {
            session_id,
            next_sequence: 1,
            cumulative_ack: 0,
            pending: [None; SPI_TX_WINDOW],
        }
    }

    pub const fn session_id(&self) -> u32 {
        self.session_id
    }

    pub fn pending_len(&self) -> usize {
        self.pending.iter().flatten().count()
    }

    pub fn reset_session(&mut self, session_id: u32) {
        self.session_id = session_id;
        self.next_sequence = 1;
        self.cumulative_ack = 0;
        self.pending = [None; SPI_TX_WINDOW];
    }

    pub fn discard_pending(&mut self) -> usize {
        let discarded = self.pending_len();
        self.pending = [None; SPI_TX_WINDOW];
        discarded
    }

    pub fn queue(
        &mut self,
        payload: &[u8],
        record_count: u8,
        now_ms: u64,
    ) -> Result<SpiCell, SenderError> {
        if payload.len() > SPI_CELL_PAYLOAD_LEN {
            return Err(SenderError::PayloadTooLong);
        }
        let Some(slot) = self.pending.iter().position(Option::is_none) else {
            return Err(SenderError::WindowFull);
        };
        let sequence = self.next_sequence;
        self.next_sequence = next_sequence(sequence);
        let mut cell = SpiCell::empty(self.session_id);
        cell.header.tx_sequence = sequence;
        cell.header.cumulative_ack = self.cumulative_ack;
        cell.header.payload_len = payload.len() as u16;
        cell.header.record_count = record_count;
        cell.payload[..payload.len()].copy_from_slice(payload);
        self.pending[slot] = Some(PendingCell {
            cell,
            last_sent_ms: now_ms,
            attempts: 1,
        });
        Ok(cell)
    }

    pub fn acknowledge(&mut self, cumulative_ack: u16) -> usize {
        let Some(index) = self.pending.iter().position(|pending| {
            pending.is_some_and(|pending| pending.cell.header.tx_sequence == cumulative_ack)
        }) else {
            return 0;
        };
        let removed = index + 1;
        for destination in 0..SPI_TX_WINDOW {
            self.pending[destination] = self.pending.get(destination + removed).copied().flatten();
        }
        removed
    }

    pub fn set_cumulative_ack(&mut self, cumulative_ack: u16) {
        self.cumulative_ack = cumulative_ack;
        for pending in self.pending.iter_mut().flatten() {
            pending.cell.header.cumulative_ack = cumulative_ack;
        }
    }

    pub fn poll_retransmit(
        &mut self,
        now_ms: u64,
        timeout_ms: u64,
        max_attempts: u8,
    ) -> RetransmitAction {
        let Some(pending) = self.pending.iter_mut().flatten().next() else {
            return RetransmitAction::Idle;
        };
        if now_ms.saturating_sub(pending.last_sent_ms) < timeout_ms {
            return RetransmitAction::Idle;
        }
        if pending.attempts >= max_attempts {
            return RetransmitAction::LinkResetRequired;
        }
        pending.attempts = pending.attempts.saturating_add(1);
        pending.last_sent_ms = now_ms;
        RetransmitAction::Send(pending.cell)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SenderError {
    PayloadTooLong,
    WindowFull,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetransmitAction {
    Idle,
    Send(SpiCell),
    LinkResetRequired,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReliableReceiver {
    session_id: Option<u32>,
    cumulative_ack: u16,
}

impl ReliableReceiver {
    pub const fn new() -> Self {
        Self {
            session_id: None,
            cumulative_ack: 0,
        }
    }

    pub const fn cumulative_ack(self) -> u16 {
        self.cumulative_ack
    }

    pub const fn session_id(self) -> Option<u32> {
        self.session_id
    }

    pub fn reset_session(&mut self, session_id: u32) {
        self.session_id = Some(session_id);
        self.cumulative_ack = 0;
    }

    pub fn receive(&mut self, cell: &SpiCell) -> ReceiveDisposition {
        let session_changed = self.session_id != Some(cell.header.session_id);
        if session_changed {
            self.session_id = Some(cell.header.session_id);
            self.cumulative_ack = 0;
        }
        if cell.header.tx_sequence == 0 {
            return if session_changed {
                ReceiveDisposition::SessionChanged
            } else {
                ReceiveDisposition::Empty
            };
        }
        let expected = next_sequence(self.cumulative_ack);
        if cell.header.tx_sequence == expected {
            self.cumulative_ack = cell.header.tx_sequence;
            ReceiveDisposition::Accepted {
                session_changed,
                cumulative_ack: self.cumulative_ack,
            }
        } else if is_recent_sequence(cell.header.tx_sequence, self.cumulative_ack) {
            ReceiveDisposition::Duplicate {
                cumulative_ack: self.cumulative_ack,
            }
        } else {
            ReceiveDisposition::Gap {
                expected,
                cumulative_ack: self.cumulative_ack,
            }
        }
    }
}

impl Default for ReliableReceiver {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveDisposition {
    Empty,
    SessionChanged,
    Accepted {
        session_changed: bool,
        cumulative_ack: u16,
    },
    Duplicate {
        cumulative_ack: u16,
    },
    Gap {
        expected: u16,
        cumulative_ack: u16,
    },
}

const fn next_sequence(sequence: u16) -> u16 {
    if sequence == u16::MAX || sequence == 0 {
        1
    } else {
        sequence + 1
    }
}

const fn previous_sequence(sequence: u16) -> u16 {
    if sequence <= 1 {
        u16::MAX
    } else {
        sequence - 1
    }
}

fn is_recent_sequence(candidate: u16, cumulative_ack: u16) -> bool {
    let mut recent = cumulative_ack;
    for _ in 0..SPI_TX_WINDOW {
        if candidate == recent {
            return true;
        }
        recent = previous_sequence(recent);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_survives_session_reset_and_replacement_hello_ack() {
        let mut slot = ReliableCommandSlot::new();
        slot.stage(7u8);
        slot.mark_queued();
        slot.sender_reset();

        assert_eq!(slot.acknowledge(true), None);
        assert_eq!(slot.value(), Some(7));

        slot.mark_queued();
        assert_eq!(slot.acknowledge(false), None);
        assert_eq!(slot.value(), Some(7));
        slot.mark_queued();
        assert_eq!(slot.acknowledge(true), Some(7));
        assert_eq!(slot.value(), None);

        slot.stage(9);
        assert_eq!(slot.complete(), Some(9));
        assert_eq!(slot.value(), None);
    }

    #[test]
    fn semantic_ack_can_discard_pending_transport_cell() {
        let mut sender = ReliableSender::new(1);
        sender.queue(&[1], 1, 0).unwrap();
        assert_eq!(sender.pending_len(), 1);
        assert_eq!(sender.discard_pending(), 1);
        assert_eq!(sender.pending_len(), 0);
    }

    #[test]
    fn four_cell_window_and_cumulative_ack_release_prefix() {
        let mut sender = ReliableSender::new(7);
        for expected in 1..=4 {
            assert_eq!(
                sender
                    .queue(&[expected as u8], 1, 0)
                    .unwrap()
                    .header
                    .tx_sequence,
                expected
            );
        }
        assert_eq!(sender.queue(&[5], 1, 0), Err(SenderError::WindowFull));
        assert_eq!(sender.acknowledge(2), 2);
        assert_eq!(sender.pending_len(), 2);
        assert_eq!(sender.queue(&[5], 1, 0).unwrap().header.tx_sequence, 5);
    }

    #[test]
    fn receiver_deduplicates_and_does_not_ack_across_gap() {
        let mut receiver = ReliableReceiver::new();
        let mut first = SpiCell::empty(9);
        first.header.tx_sequence = 1;
        assert_eq!(
            receiver.receive(&first),
            ReceiveDisposition::Accepted {
                session_changed: true,
                cumulative_ack: 1
            }
        );
        assert_eq!(
            receiver.receive(&first),
            ReceiveDisposition::Duplicate { cumulative_ack: 1 }
        );
        let mut second = first;
        second.header.tx_sequence = 2;
        assert!(matches!(
            receiver.receive(&second),
            ReceiveDisposition::Accepted {
                cumulative_ack: 2,
                ..
            }
        ));
        assert_eq!(
            receiver.receive(&first),
            ReceiveDisposition::Duplicate { cumulative_ack: 2 }
        );
        let mut third = first;
        third.header.tx_sequence = 3;
        let mut fourth = first;
        fourth.header.tx_sequence = 4;
        assert_eq!(
            receiver.receive(&fourth),
            ReceiveDisposition::Gap {
                expected: 3,
                cumulative_ack: 2
            }
        );
        assert_eq!(receiver.cumulative_ack(), 2);
        assert_eq!(
            receiver.receive(&third),
            ReceiveDisposition::Accepted {
                session_changed: false,
                cumulative_ack: 3
            }
        );
    }

    #[test]
    fn retries_eventually_require_link_reset_without_double_delivery() {
        let mut sender = ReliableSender::new(1);
        let original = sender.queue(&[1, 2], 1, 10).unwrap();
        assert_eq!(sender.poll_retransmit(19, 10, 2), RetransmitAction::Idle);
        assert_eq!(
            sender.poll_retransmit(20, 10, 2),
            RetransmitAction::Send(original)
        );
        assert_eq!(
            sender.poll_retransmit(30, 10, 2),
            RetransmitAction::LinkResetRequired
        );
    }

    #[test]
    fn session_change_discards_receive_history() {
        let mut receiver = ReliableReceiver::new();
        let mut old = SpiCell::empty(1);
        old.header.tx_sequence = 1;
        receiver.receive(&old);
        let mut new = SpiCell::empty(2);
        new.header.tx_sequence = 1;
        assert_eq!(
            receiver.receive(&new),
            ReceiveDisposition::Accepted {
                session_changed: true,
                cumulative_ack: 1
            }
        );
    }

    #[test]
    fn sender_session_change_discards_every_pending_cell() {
        let mut sender = ReliableSender::new(1);
        sender.queue(&[1], 1, 0).unwrap();
        sender.queue(&[2], 1, 0).unwrap();
        sender.reset_session(9);
        assert_eq!(sender.session_id(), 9);
        assert_eq!(sender.pending_len(), 0);
        assert_eq!(sender.queue(&[3], 1, 0).unwrap().header.tx_sequence, 1);
    }

    #[test]
    fn sequence_wrap_skips_reserved_zero() {
        let mut receiver = ReliableReceiver {
            session_id: Some(1),
            cumulative_ack: u16::MAX,
        };
        let mut wrapped = SpiCell::empty(1);
        wrapped.header.tx_sequence = 1;
        assert_eq!(
            receiver.receive(&wrapped),
            ReceiveDisposition::Accepted {
                session_changed: false,
                cumulative_ack: 1
            }
        );
    }

    #[test]
    fn sender_sequence_wrap_skips_reserved_zero() {
        let mut sender = ReliableSender::new(1);
        sender.next_sequence = u16::MAX;
        assert_eq!(
            sender.queue(&[1], 1, 0).unwrap().header.tx_sequence,
            u16::MAX
        );
        assert_eq!(sender.queue(&[2], 1, 0).unwrap().header.tx_sequence, 1);
    }
}
