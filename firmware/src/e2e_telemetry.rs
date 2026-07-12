//! Non-blocking timing capture for the hardware E2E feature.
//!
//! Values are read only after the corresponding BLE notification arrives, so
//! instrumentation does not write to UART on the measured input path.

use portable_atomic::{AtomicU32, AtomicU64, Ordering};

static INPUT_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static INGRESS_US: AtomicU64 = AtomicU64::new(0);
static RUNTIME_US: AtomicU64 = AtomicU64::new(0);
static RUNTIME_DISPATCH_US: AtomicU64 = AtomicU64::new(0);
static BLE_QUEUED_US: AtomicU64 = AtomicU64::new(0);
static BLE_RECEIVE_US: AtomicU64 = AtomicU64::new(0);
static NOTIFY_START_US: AtomicU64 = AtomicU64::new(0);
static NOTIFY_DONE_US: AtomicU64 = AtomicU64::new(0);
static INPUT_COUNT: AtomicU32 = AtomicU32::new(0);
static BLE_QUEUED_COUNT: AtomicU32 = AtomicU32::new(0);
static NOTIFY_DONE_COUNT: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
pub struct Snapshot {
    pub sequence: u32,
    pub ingress_us: u64,
    pub runtime_us: u64,
    pub runtime_dispatch_us: u64,
    pub ble_queued_us: u64,
    pub ble_receive_us: u64,
    pub notify_start_us: u64,
    pub notify_done_us: u64,
    pub input_count: u32,
    pub ble_queued_count: u32,
    pub notify_done_count: u32,
}

pub fn record_ingress(sequence: u32, now_us: u64) {
    INPUT_COUNT.fetch_add(1, Ordering::Relaxed);
    INGRESS_US.store(now_us, Ordering::Relaxed);
    RUNTIME_US.store(0, Ordering::Relaxed);
    RUNTIME_DISPATCH_US.store(0, Ordering::Relaxed);
    BLE_QUEUED_US.store(0, Ordering::Relaxed);
    BLE_RECEIVE_US.store(0, Ordering::Relaxed);
    NOTIFY_START_US.store(0, Ordering::Relaxed);
    NOTIFY_DONE_US.store(0, Ordering::Relaxed);
    INPUT_SEQUENCE.store(sequence, Ordering::Release);
}

pub fn record_runtime(now_us: u64) {
    RUNTIME_US.store(now_us, Ordering::Relaxed);
}

pub fn record_runtime_dispatch(now_us: u64) {
    RUNTIME_DISPATCH_US.store(now_us, Ordering::Relaxed);
}

pub fn record_ble_queued(now_us: u64) {
    BLE_QUEUED_COUNT.fetch_add(1, Ordering::Relaxed);
    BLE_QUEUED_US.store(now_us, Ordering::Relaxed);
}

pub fn record_ble_receive(now_us: u64) {
    BLE_RECEIVE_US.store(now_us, Ordering::Relaxed);
}

pub fn record_notify_start(now_us: u64) {
    NOTIFY_START_US.store(now_us, Ordering::Relaxed);
}

pub fn record_notify_done(now_us: u64) {
    NOTIFY_DONE_COUNT.fetch_add(1, Ordering::Relaxed);
    NOTIFY_DONE_US.store(now_us, Ordering::Relaxed);
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        sequence: INPUT_SEQUENCE.load(Ordering::Acquire),
        ingress_us: INGRESS_US.load(Ordering::Relaxed),
        runtime_us: RUNTIME_US.load(Ordering::Relaxed),
        runtime_dispatch_us: RUNTIME_DISPATCH_US.load(Ordering::Relaxed),
        ble_queued_us: BLE_QUEUED_US.load(Ordering::Relaxed),
        ble_receive_us: BLE_RECEIVE_US.load(Ordering::Relaxed),
        notify_start_us: NOTIFY_START_US.load(Ordering::Relaxed),
        notify_done_us: NOTIFY_DONE_US.load(Ordering::Relaxed),
        input_count: INPUT_COUNT.load(Ordering::Relaxed),
        ble_queued_count: BLE_QUEUED_COUNT.load(Ordering::Relaxed),
        notify_done_count: NOTIFY_DONE_COUNT.load(Ordering::Relaxed),
    }
}
