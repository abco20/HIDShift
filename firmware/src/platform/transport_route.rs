use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use hidshift::{InputTransport, InputTransportRouter};

#[cfg(not(feature = "espnow"))]
const BLE: u8 = InputTransport::Ble as u8;
const ESPNOW: u8 = InputTransport::EspNow as u8;

#[cfg(feature = "espnow")]
const INITIAL_TRANSPORT: u8 = ESPNOW;
#[cfg(not(feature = "espnow"))]
const INITIAL_TRANSPORT: u8 = BLE;

static SELECTED: AtomicU8 = AtomicU8::new(INITIAL_TRANSPORT);
static BLE_AVAILABLE: AtomicBool = AtomicBool::new(false);
static ESPNOW_AVAILABLE: AtomicBool = AtomicBool::new(false);

pub fn select(transport: InputTransport) {
    SELECTED.store(transport as u8, Ordering::Release);
}

pub fn selected() -> InputTransport {
    decode(SELECTED.load(Ordering::Acquire))
}

pub fn set_available(transport: InputTransport, available: bool) {
    match transport {
        InputTransport::Ble => BLE_AVAILABLE.store(available, Ordering::Release),
        InputTransport::EspNow => ESPNOW_AVAILABLE.store(available, Ordering::Release),
    }
}

pub fn active() -> Option<InputTransport> {
    let mut router = InputTransportRouter::new(selected());
    router.set_available(InputTransport::Ble, BLE_AVAILABLE.load(Ordering::Acquire));
    router.set_available(
        InputTransport::EspNow,
        ESPNOW_AVAILABLE.load(Ordering::Acquire),
    );
    router.active()
}

pub fn routes_to(transport: InputTransport) -> bool {
    active() == Some(transport)
}

const fn decode(value: u8) -> InputTransport {
    if value == ESPNOW {
        InputTransport::EspNow
    } else {
        InputTransport::Ble
    }
}
