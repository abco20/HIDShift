use portable_atomic::{AtomicU32, Ordering};

static TX_CRC_FAILURES_REMAINING: AtomicU32 = AtomicU32::new(0);
static LAST_CORRUPTED_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static TX_CRC_FAILURES_INJECTED: AtomicU32 = AtomicU32::new(0);
static TX_CRC_RETRANSMISSIONS_OBSERVED: AtomicU32 = AtomicU32::new(0);
static SPI_CELLS_TO_DROP: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiFaultSnapshot {
    pub crc_failures_remaining: u32,
    pub crc_failures_injected: u32,
    pub crc_retransmissions_observed: u32,
}

pub fn request_tx_crc_failures(count: u32) {
    TX_CRC_FAILURES_REMAINING.fetch_add(count, Ordering::Relaxed);
}

pub fn request_spi_cell_drops(count: u32) {
    let _ = SPI_CELLS_TO_DROP.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
        Some(current.saturating_add(count))
    });
}

pub fn consume_spi_cell_drop() -> bool {
    consume_one(&SPI_CELLS_TO_DROP)
}

pub fn corrupt_tx_if_requested(sequence: u16, encoded_cell: &mut [u8]) -> bool {
    if sequence == 0 || encoded_cell.is_empty() || !consume_one(&TX_CRC_FAILURES_REMAINING) {
        return false;
    }
    let last = encoded_cell.len() - 1;
    encoded_cell[last] ^= 1;
    LAST_CORRUPTED_SEQUENCE.store(u32::from(sequence), Ordering::Relaxed);
    TX_CRC_FAILURES_INJECTED.fetch_add(1, Ordering::Relaxed);
    true
}

pub fn observe_retransmission(sequence: u16) -> bool {
    if LAST_CORRUPTED_SEQUENCE
        .compare_exchange(u32::from(sequence), 0, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    TX_CRC_RETRANSMISSIONS_OBSERVED.fetch_add(1, Ordering::Relaxed);
    true
}

pub fn snapshot() -> SpiFaultSnapshot {
    SpiFaultSnapshot {
        crc_failures_remaining: TX_CRC_FAILURES_REMAINING.load(Ordering::Relaxed),
        crc_failures_injected: TX_CRC_FAILURES_INJECTED.load(Ordering::Relaxed),
        crc_retransmissions_observed: TX_CRC_RETRANSMISSIONS_OBSERVED.load(Ordering::Relaxed),
    }
}

pub fn reset() {
    TX_CRC_FAILURES_REMAINING.store(0, Ordering::Relaxed);
    LAST_CORRUPTED_SEQUENCE.store(0, Ordering::Relaxed);
    TX_CRC_FAILURES_INJECTED.store(0, Ordering::Relaxed);
    TX_CRC_RETRANSMISSIONS_OBSERVED.store(0, Ordering::Relaxed);
    SPI_CELLS_TO_DROP.store(0, Ordering::Relaxed);
}

fn consume_one(counter: &AtomicU32) -> bool {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_sub(1)
        })
        .is_ok()
}
