pub mod ble_consumer;
pub mod ble_hid;
pub mod ble_keyboard;
pub mod ble_mouse;

pub use ble_consumer::{BleConsumerReport, CONSUMER_REPORT_ID, CONSUMER_REPORT_LEN};
pub use ble_hid::{
    BLE_HID_INPUT_REPORT_MAX_LEN, BLE_HID_NOTIFICATIONS_PER_REPORT_MAX, BLE_HID_NOTIFY_MAX_LEN,
    BleHidCharacteristic, BleHidInputReport, BleHidNotification, BleHidNotificationError,
    FEATURE_REPORT_TYPE, HID_INFORMATION, INPUT_REPORT_TYPE, OUTPUT_REPORT_TYPE,
    V1_CONSUMER_REPORT_MAP, V1_KEYBOARD_REPORT_MAP, V1_MOUSE_REPORT_MAP,
    notifications_for_input_report, report_id, report_type,
};
pub use ble_keyboard::{
    BleKeyboard6KroReport, BleKeyboardLedOutputReport, BleKeyboardOutputError, BleKeyboardReport,
    KEYBOARD_LED_OUTPUT_REPORT_LEN, KEYBOARD_REPORT_ID, KEYBOARD_REPORT_LEN, KeyboardReportBuild,
};
pub use ble_mouse::{BleMouseReport, MOUSE_REPORT_ID, MOUSE_REPORT_LEN};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleHidReport {
    Keyboard(BleKeyboard6KroReport),
    Mouse(BleMouseReport),
    Consumer(BleConsumerReport),
}

impl BleHidReport {
    pub const fn kind(self) -> ReportKind {
        match self {
            Self::Keyboard(_) => ReportKind::Keyboard,
            Self::Mouse(_) => ReportKind::Mouse,
            Self::Consumer(_) => ReportKind::Consumer,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReportKind {
    Keyboard,
    Mouse,
    Consumer,
    KeyboardOutput,
}
