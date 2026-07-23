use hidshift::remote_wakeup::RemoteWakeupAction;

const DCTL_OFFSET: usize = 0x804;
const PCGCCTL_OFFSET: usize = 0xe00;
const REMOTE_WAKEUP_SIGNAL: u32 = 1 << 0;
const SOFT_DISCONNECT: u32 = 1 << 1;
const STOP_PHY_CLOCK: u32 = 1 << 0;
const GATE_AHB_CLOCK: u32 = 1 << 1;

pub fn apply(action: RemoteWakeupAction) {
    match action {
        RemoteWakeupAction::AssertSignal => assert_signal(),
        RemoteWakeupAction::ClearSignal => clear_signal(),
    }
}

pub fn soft_disconnect() {
    let dctl = register(DCTL_OFFSET);
    // SAFETY: the caller stops all subsequent USB access and resets the CPU.
    unsafe {
        dctl.write_volatile(dctl.read_volatile() | SOFT_DISCONNECT);
    }
    // A 5 ms gap was intermittently cached as the old VID/PID during dynamic
    // Profile A -> B switching, so use a conservative disconnect interval.
    esp_hal::delay::Delay::new().delay_millis(100);
}

fn assert_signal() {
    let pcgcctl = register(PCGCCTL_OFFSET);
    let dctl = register(DCTL_OFFSET);
    // SAFETY: the Device firmware exclusively owns USB0. This code runs from
    // the same blocking task as usb-device polling, so these read/modify/write
    // operations cannot race another owner of the peripheral.
    unsafe {
        pcgcctl.write_volatile(pcgcctl.read_volatile() & !(STOP_PHY_CLOCK | GATE_AHB_CLOCK));
        dctl.write_volatile(dctl.read_volatile() | REMOTE_WAKEUP_SIGNAL);
    }
}

fn clear_signal() {
    let dctl = register(DCTL_OFFSET);
    // SAFETY: see assert_signal. Only the remote-wakeup bit is modified.
    unsafe {
        dctl.write_volatile(dctl.read_volatile() & !REMOTE_WAKEUP_SIGNAL);
    }
}

#[cfg(feature = "hardware-e2e")]
fn signal_asserted() -> bool {
    let dctl = register(DCTL_OFFSET);
    // SAFETY: USB0 is clocked and owned by this firmware.
    unsafe { dctl.read_volatile() & REMOTE_WAKEUP_SIGNAL != 0 }
}

fn register(offset: usize) -> *mut u32 {
    (esp_hal::peripherals::USB0::PTR as usize + offset) as *mut u32
}

#[cfg(feature = "hardware-e2e")]
pub fn run_hardware_self_test() {
    clear_signal();
    let clear_before = !signal_asserted();
    assert_signal();
    let asserted = signal_asserted();
    esp_hal::delay::Delay::new().delay_millis(10);
    clear_signal();
    let cleared = !signal_asserted();
    log::info!(
        "@HIDSHIFT-REMOTE-WAKE:clear-before={},asserted={},cleared={}",
        clear_before,
        asserted,
        cleared
    );
}
