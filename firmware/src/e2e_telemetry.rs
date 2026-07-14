//! Non-blocking timing capture for the hardware E2E feature.
//!
//! Values are read only after the corresponding BLE notification arrives, so
//! instrumentation does not write to UART on the measured input path. Timing
//! storage uses the native 32-bit atomic width: a hardware E2E run completes
//! well inside the 2^32-microsecond (about 71-minute) wrap interval, and the
//! snapshot widens values back to the stable u64 UART protocol.

use portable_atomic::{AtomicU32, Ordering};

static INPUT_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static INGRESS_US: AtomicU32 = AtomicU32::new(0);
static RUNTIME_US: AtomicU32 = AtomicU32::new(0);
static RUNTIME_DISPATCH_US: AtomicU32 = AtomicU32::new(0);
static BLE_QUEUED_US: AtomicU32 = AtomicU32::new(0);
static BLE_RECEIVE_US: AtomicU32 = AtomicU32::new(0);
static NOTIFY_START_US: AtomicU32 = AtomicU32::new(0);
static NOTIFY_DONE_US: AtomicU32 = AtomicU32::new(0);
static HCI_SUBMIT_US: AtomicU32 = AtomicU32::new(0);
static HCI_DEQUEUE_US: AtomicU32 = AtomicU32::new(0);
static HCI_CREDIT_US: AtomicU32 = AtomicU32::new(0);
static INPUT_COUNT: AtomicU32 = AtomicU32::new(0);
static BLE_QUEUED_COUNT: AtomicU32 = AtomicU32::new(0);
static NOTIFY_DONE_COUNT: AtomicU32 = AtomicU32::new(0);
static BLE_CONNECTED: AtomicU32 = AtomicU32::new(0);
static BLE_CONNECTION_INTERVAL_US: AtomicU32 = AtomicU32::new(0);
static BLE_PERIPHERAL_LATENCY: AtomicU32 = AtomicU32::new(0);
static BLE_SUPERVISION_TIMEOUT_MS: AtomicU32 = AtomicU32::new(0);
static BLE_TX_PHY: AtomicU32 = AtomicU32::new(0);
static BLE_RX_PHY: AtomicU32 = AtomicU32::new(0);
static BLE_PARAMETER_UPDATES: AtomicU32 = AtomicU32::new(0);
static BLE_PHY_UPDATES: AtomicU32 = AtomicU32::new(0);
const ESPNOW_TIMING_CAPACITY: usize = 32;

struct EspNowTimingSlot {
    sequence: AtomicU32,
    ingress_us: AtomicU32,
    enqueue_us: AtomicU32,
    dequeue_us: AtomicU32,
    send_start_us: AtomicU32,
    tx_done_us: AtomicU32,
}

impl EspNowTimingSlot {
    const fn new() -> Self {
        Self {
            sequence: AtomicU32::new(0),
            ingress_us: AtomicU32::new(0),
            enqueue_us: AtomicU32::new(0),
            dequeue_us: AtomicU32::new(0),
            send_start_us: AtomicU32::new(0),
            tx_done_us: AtomicU32::new(0),
        }
    }
}

static ESPNOW_TIMINGS: [EspNowTimingSlot; ESPNOW_TIMING_CAPACITY] =
    [const { EspNowTimingSlot::new() }; ESPNOW_TIMING_CAPACITY];
static LATEST_ESPNOW_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static DEVICE_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static DEVICE_RADIO_RX_US: AtomicU32 = AtomicU32::new(0);
static DEVICE_REASSEMBLED_US: AtomicU32 = AtomicU32::new(0);
static DEVICE_HID_WRITE_US: AtomicU32 = AtomicU32::new(0);

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
    pub hci_submit_us: u64,
    pub hci_dequeue_us: u64,
    pub hci_credit_us: u64,
    pub input_count: u32,
    pub ble_queued_count: u32,
    pub notify_done_count: u32,
    pub ble_connected: bool,
    pub ble_connection_interval_us: u32,
    pub ble_peripheral_latency: u16,
    pub ble_supervision_timeout_ms: u32,
    pub ble_tx_phy: u8,
    pub ble_rx_phy: u8,
    pub ble_parameter_updates: u32,
    pub ble_phy_updates: u32,
    pub espnow_sequence: u32,
    pub espnow_ingress_us: u64,
    pub espnow_enqueue_us: u64,
    pub espnow_dequeue_us: u64,
    pub espnow_send_start_us: u64,
    pub espnow_tx_done_us: u64,
    pub device_sequence: u32,
    pub device_radio_rx_us: u64,
    pub device_reassembled_us: u64,
    pub device_hid_write_us: u64,
}

pub fn record_ingress(sequence: u32, now_us: u64) {
    INPUT_COUNT.fetch_add(1, Ordering::Relaxed);
    INGRESS_US.store(timestamp32(now_us), Ordering::Relaxed);
    RUNTIME_US.store(0, Ordering::Relaxed);
    RUNTIME_DISPATCH_US.store(0, Ordering::Relaxed);
    BLE_QUEUED_US.store(0, Ordering::Relaxed);
    BLE_RECEIVE_US.store(0, Ordering::Relaxed);
    NOTIFY_START_US.store(0, Ordering::Relaxed);
    NOTIFY_DONE_US.store(0, Ordering::Relaxed);
    HCI_SUBMIT_US.store(0, Ordering::Relaxed);
    HCI_DEQUEUE_US.store(0, Ordering::Relaxed);
    HCI_CREDIT_US.store(0, Ordering::Relaxed);
    INPUT_SEQUENCE.store(sequence, Ordering::Release);
}

pub fn record_runtime(now_us: u64) {
    RUNTIME_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_runtime_dispatch(now_us: u64) {
    RUNTIME_DISPATCH_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_ble_queued(now_us: u64) {
    BLE_QUEUED_COUNT.fetch_add(1, Ordering::Relaxed);
    BLE_QUEUED_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_ble_receive(now_us: u64) {
    BLE_RECEIVE_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_notify_start(now_us: u64) {
    NOTIFY_START_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_notify_done(now_us: u64) {
    NOTIFY_DONE_COUNT.fetch_add(1, Ordering::Relaxed);
    NOTIFY_DONE_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_hci_submit(now_us: u64) {
    HCI_SUBMIT_US.store(timestamp32(now_us), Ordering::Release);
}

pub fn record_hci_dequeue(now_us: u64) {
    HCI_DEQUEUE_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_hci_credit(now_us: u64) {
    HCI_CREDIT_US.store(timestamp32(now_us), Ordering::Relaxed);
}

pub fn record_ble_connected(
    connection_interval_us: u32,
    peripheral_latency: u16,
    supervision_timeout_ms: u32,
) {
    BLE_CONNECTION_INTERVAL_US.store(connection_interval_us, Ordering::Relaxed);
    BLE_PERIPHERAL_LATENCY.store(u32::from(peripheral_latency), Ordering::Relaxed);
    BLE_SUPERVISION_TIMEOUT_MS.store(supervision_timeout_ms, Ordering::Relaxed);
    BLE_TX_PHY.store(1, Ordering::Relaxed);
    BLE_RX_PHY.store(1, Ordering::Relaxed);
    BLE_CONNECTED.store(1, Ordering::Release);
}

pub fn record_ble_connection_parameters(
    connection_interval_us: u32,
    peripheral_latency: u16,
    supervision_timeout_ms: u32,
) {
    BLE_CONNECTION_INTERVAL_US.store(connection_interval_us, Ordering::Relaxed);
    BLE_PERIPHERAL_LATENCY.store(u32::from(peripheral_latency), Ordering::Relaxed);
    BLE_SUPERVISION_TIMEOUT_MS.store(supervision_timeout_ms, Ordering::Relaxed);
    BLE_PARAMETER_UPDATES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_ble_phy(tx_phy: u8, rx_phy: u8) {
    BLE_TX_PHY.store(u32::from(tx_phy), Ordering::Relaxed);
    BLE_RX_PHY.store(u32::from(rx_phy), Ordering::Relaxed);
    BLE_PHY_UPDATES.fetch_add(1, Ordering::Relaxed);
}

pub fn record_ble_disconnected() {
    BLE_CONNECTED.store(0, Ordering::Release);
}

pub fn record_espnow_enqueue(sequence: u32, ingress_us: u64, enqueue_us: u64) {
    let slot = espnow_timing_slot(sequence);
    slot.sequence.store(0, Ordering::Release);
    slot.ingress_us
        .store(timestamp32(ingress_us), Ordering::Relaxed);
    slot.enqueue_us
        .store(timestamp32(enqueue_us), Ordering::Relaxed);
    slot.dequeue_us.store(0, Ordering::Relaxed);
    slot.send_start_us.store(0, Ordering::Relaxed);
    slot.tx_done_us.store(0, Ordering::Relaxed);
    slot.sequence.store(sequence, Ordering::Release);
    LATEST_ESPNOW_SEQUENCE.store(sequence, Ordering::Release);
}

pub fn reset_espnow_timings() {
    LATEST_ESPNOW_SEQUENCE.store(0, Ordering::Release);
    for slot in &ESPNOW_TIMINGS {
        slot.sequence.store(0, Ordering::Release);
        slot.ingress_us.store(0, Ordering::Relaxed);
        slot.enqueue_us.store(0, Ordering::Relaxed);
        slot.dequeue_us.store(0, Ordering::Relaxed);
        slot.send_start_us.store(0, Ordering::Relaxed);
        slot.tx_done_us.store(0, Ordering::Relaxed);
    }
}

pub fn record_espnow_dequeue(sequence: u32, dequeue_us: u64) {
    let slot = espnow_timing_slot(sequence);
    if slot.sequence.load(Ordering::Acquire) == sequence {
        slot.dequeue_us
            .store(timestamp32(dequeue_us), Ordering::Relaxed);
    }
}

pub fn record_espnow_tx(sequence: u32, ingress_us: u64, send_start_us: u64, tx_done_us: u64) {
    let slot = espnow_timing_slot(sequence);
    if slot.sequence.load(Ordering::Acquire) == sequence {
        slot.ingress_us
            .store(timestamp32(ingress_us), Ordering::Relaxed);
        slot.send_start_us
            .store(timestamp32(send_start_us), Ordering::Relaxed);
        slot.tx_done_us
            .store(timestamp32(tx_done_us), Ordering::Release);
    }
}

fn espnow_timing_slot(sequence: u32) -> &'static EspNowTimingSlot {
    &ESPNOW_TIMINGS[sequence as usize % ESPNOW_TIMING_CAPACITY]
}

pub fn record_device_bridge_timing(
    sequence: u32,
    radio_rx_us: u64,
    reassembled_us: u64,
    hid_write_us: u64,
) {
    DEVICE_RADIO_RX_US.store(timestamp32(radio_rx_us), Ordering::Relaxed);
    DEVICE_REASSEMBLED_US.store(timestamp32(reassembled_us), Ordering::Relaxed);
    DEVICE_HID_WRITE_US.store(timestamp32(hid_write_us), Ordering::Relaxed);
    DEVICE_SEQUENCE.store(sequence, Ordering::Release);
}

pub fn snapshot(target_espnow_sequence: u32) -> Snapshot {
    let requested_sequence = if target_espnow_sequence == 0 {
        LATEST_ESPNOW_SEQUENCE.load(Ordering::Acquire)
    } else {
        target_espnow_sequence
    };
    let espnow = espnow_timing_slot(requested_sequence);
    let espnow_sequence = espnow.sequence.load(Ordering::Acquire);
    Snapshot {
        sequence: INPUT_SEQUENCE.load(Ordering::Acquire),
        ingress_us: load_timestamp(&INGRESS_US, Ordering::Relaxed),
        runtime_us: load_timestamp(&RUNTIME_US, Ordering::Relaxed),
        runtime_dispatch_us: load_timestamp(&RUNTIME_DISPATCH_US, Ordering::Relaxed),
        ble_queued_us: load_timestamp(&BLE_QUEUED_US, Ordering::Relaxed),
        ble_receive_us: load_timestamp(&BLE_RECEIVE_US, Ordering::Relaxed),
        notify_start_us: load_timestamp(&NOTIFY_START_US, Ordering::Relaxed),
        notify_done_us: load_timestamp(&NOTIFY_DONE_US, Ordering::Relaxed),
        hci_submit_us: load_timestamp(&HCI_SUBMIT_US, Ordering::Acquire),
        hci_dequeue_us: load_timestamp(&HCI_DEQUEUE_US, Ordering::Relaxed),
        hci_credit_us: load_timestamp(&HCI_CREDIT_US, Ordering::Relaxed),
        input_count: INPUT_COUNT.load(Ordering::Relaxed),
        ble_queued_count: BLE_QUEUED_COUNT.load(Ordering::Relaxed),
        notify_done_count: NOTIFY_DONE_COUNT.load(Ordering::Relaxed),
        ble_connected: BLE_CONNECTED.load(Ordering::Acquire) != 0,
        ble_connection_interval_us: BLE_CONNECTION_INTERVAL_US.load(Ordering::Relaxed),
        ble_peripheral_latency: BLE_PERIPHERAL_LATENCY.load(Ordering::Relaxed) as u16,
        ble_supervision_timeout_ms: BLE_SUPERVISION_TIMEOUT_MS.load(Ordering::Relaxed),
        ble_tx_phy: BLE_TX_PHY.load(Ordering::Relaxed) as u8,
        ble_rx_phy: BLE_RX_PHY.load(Ordering::Relaxed) as u8,
        ble_parameter_updates: BLE_PARAMETER_UPDATES.load(Ordering::Relaxed),
        ble_phy_updates: BLE_PHY_UPDATES.load(Ordering::Relaxed),
        espnow_sequence,
        espnow_ingress_us: load_timestamp(&espnow.ingress_us, Ordering::Relaxed),
        espnow_enqueue_us: load_timestamp(&espnow.enqueue_us, Ordering::Relaxed),
        espnow_dequeue_us: load_timestamp(&espnow.dequeue_us, Ordering::Relaxed),
        espnow_send_start_us: load_timestamp(&espnow.send_start_us, Ordering::Relaxed),
        espnow_tx_done_us: load_timestamp(&espnow.tx_done_us, Ordering::Acquire),
        device_sequence: DEVICE_SEQUENCE.load(Ordering::Acquire),
        device_radio_rx_us: load_timestamp(&DEVICE_RADIO_RX_US, Ordering::Relaxed),
        device_reassembled_us: load_timestamp(&DEVICE_REASSEMBLED_US, Ordering::Relaxed),
        device_hid_write_us: load_timestamp(&DEVICE_HID_WRITE_US, Ordering::Relaxed),
    }
}

const fn timestamp32(now_us: u64) -> u32 {
    now_us as u32
}

fn load_timestamp(value: &AtomicU32, ordering: Ordering) -> u64 {
    u64::from(value.load(ordering))
}
