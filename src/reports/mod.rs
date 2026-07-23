pub mod ble_consumer;
pub mod ble_hid;
pub mod ble_keyboard;
pub mod ble_mouse;

pub use ble_consumer::{
    BleConsumerReport, CONSUMER_REPORT_ID, CONSUMER_REPORT_LEN, ConsumerReport,
};
pub use ble_hid::{
    BLE_HID_INPUT_REPORT_MAX_LEN, BLE_HID_NOTIFICATIONS_PER_REPORT_MAX, BLE_HID_NOTIFY_MAX_LEN,
    BleHidCharacteristic, BleHidInputReport, BleHidNotification, BleHidNotificationError,
    FEATURE_REPORT_TYPE, HID_INFORMATION, INPUT_REPORT_TYPE, OUTPUT_REPORT_TYPE,
    V1_COMBINED_REPORT_MAP, notifications_for_input_report, report_id, report_type,
};
pub use ble_keyboard::{
    BleKeyboard6KroReport, BleKeyboardLedOutputReport, BleKeyboardOutputError, BleKeyboardReport,
    KEYBOARD_6KRO_KEY_CAPACITY, KEYBOARD_LED_OUTPUT_REPORT_LEN, KEYBOARD_REPORT_ID,
    KEYBOARD_REPORT_LEN, Keyboard6KroReport, KeyboardReportBuild,
};
pub use ble_mouse::{BleMouseReport, MOUSE_REPORT_ID, MOUSE_REPORT_LEN, MouseReport};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardHidReport {
    Keyboard(Keyboard6KroReport),
    Mouse(MouseReport),
    Consumer(ConsumerReport),
}

impl StandardHidReport {
    pub const fn kind(self) -> ReportKind {
        match self {
            Self::Keyboard(_) => ReportKind::Keyboard,
            Self::Mouse(_) => ReportKind::Mouse,
            Self::Consumer(_) => ReportKind::Consumer,
        }
    }

    /// Returns whether this report carries an active press or relative
    /// movement. Neutral/release reports must not wake a suspended USB host.
    pub fn has_activity(self) -> bool {
        match self {
            Self::Keyboard(report) => report.as_bytes().iter().any(|byte| *byte != 0),
            Self::Mouse(report) => report.as_bytes().iter().any(|byte| *byte != 0),
            Self::Consumer(report) => report.as_bytes().iter().any(|byte| *byte != 0),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleHidReport {
    Keyboard(BleKeyboard6KroReport),
    Mouse(BleMouseReport),
    Consumer(BleConsumerReport),
}

impl From<StandardHidReport> for BleHidReport {
    fn from(report: StandardHidReport) -> Self {
        match report {
            StandardHidReport::Keyboard(report) => Self::Keyboard(report),
            StandardHidReport::Mouse(report) => Self::Mouse(report),
            StandardHidReport::Consumer(report) => Self::Consumer(report),
        }
    }
}

impl From<BleHidReport> for StandardHidReport {
    fn from(report: BleHidReport) -> Self {
        match report {
            BleHidReport::Keyboard(report) => Self::Keyboard(report),
            BleHidReport::Mouse(report) => Self::Mouse(report),
            BleHidReport::Consumer(report) => Self::Consumer(report),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_and_release_reports_are_not_user_activity() {
        assert!(!StandardHidReport::Keyboard(Keyboard6KroReport::release()).has_activity());
        assert!(!StandardHidReport::Mouse(MouseReport::release_buttons()).has_activity());
        assert!(!StandardHidReport::Consumer(ConsumerReport::release()).has_activity());
    }

    #[test]
    fn presses_and_relative_movement_are_user_activity() {
        assert!(
            StandardHidReport::Keyboard(Keyboard6KroReport::from_bytes([0, 0, 4, 0, 0, 0, 0, 0]))
                .has_activity()
        );
        assert!(StandardHidReport::Mouse(MouseReport::from_bytes([0, 1, 0, 0, 0])).has_activity());
        assert!(StandardHidReport::Consumer(ConsumerReport::from_usage_id(0x00e9)).has_activity());
    }
}
