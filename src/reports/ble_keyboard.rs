use crate::input::{
    KeyboardLedState, KeyboardSuppression, PhysicalKeyboardState, VisibleKeyboardState,
};

pub const KEYBOARD_REPORT_ID: u8 = 1;
pub const KEYBOARD_REPORT_LEN: usize = 8;
pub const KEYBOARD_LED_OUTPUT_REPORT_LEN: usize = 1;
pub const KEYBOARD_6KRO_KEY_CAPACITY: usize = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardReportBuild {
    pub report: Keyboard6KroReport,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Keyboard6KroReport {
    bytes: [u8; KEYBOARD_REPORT_LEN],
}

pub type BleKeyboard6KroReport = Keyboard6KroReport;
pub type BleKeyboardReport = Keyboard6KroReport;

impl Keyboard6KroReport {
    pub const fn from_bytes(bytes: [u8; KEYBOARD_REPORT_LEN]) -> Self {
        Self { bytes }
    }

    pub const fn release() -> Self {
        Self {
            bytes: [0; KEYBOARD_REPORT_LEN],
        }
    }

    pub const fn empty() -> Self {
        Self::release()
    }

    pub fn from_visible_state(state: &VisibleKeyboardState) -> KeyboardReportBuild {
        let mut bytes = [0; KEYBOARD_REPORT_LEN];
        bytes[0] = state.modifiers.bits();

        let mut truncated = state.truncated();
        for (index, key) in state.keys().iter().copied().enumerate() {
            if index < KEYBOARD_6KRO_KEY_CAPACITY {
                bytes[2 + index] = key.0;
            } else {
                truncated = true;
            }
        }

        KeyboardReportBuild {
            report: Self { bytes },
            truncated,
        }
    }

    pub fn from_physical_state(
        state: &PhysicalKeyboardState,
        suppression: &KeyboardSuppression,
    ) -> KeyboardReportBuild {
        let mut bytes = [0; KEYBOARD_REPORT_LEN];
        bytes[0] = (state.modifiers & !suppression.modifiers()).bits();

        let mut visible = 0usize;
        for key in state.keys().iter().copied() {
            if suppression.contains_key(key) {
                continue;
            }
            if visible < KEYBOARD_6KRO_KEY_CAPACITY {
                bytes[2 + visible] = key.0;
            }
            visible += 1;
        }

        KeyboardReportBuild {
            report: Self { bytes },
            truncated: visible > KEYBOARD_6KRO_KEY_CAPACITY,
        }
    }

    pub const fn as_bytes(&self) -> &[u8; KEYBOARD_REPORT_LEN] {
        &self.bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleKeyboardOutputError {
    InvalidLength,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleKeyboardLedOutputReport {
    leds: KeyboardLedState,
}

impl BleKeyboardLedOutputReport {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, BleKeyboardOutputError> {
        let [bits] = bytes else {
            return Err(BleKeyboardOutputError::InvalidLength);
        };

        Ok(Self {
            leds: KeyboardLedState::from_bits_truncate(
                bits & (KeyboardLedState::NUM_LOCK
                    | KeyboardLedState::CAPS_LOCK
                    | KeyboardLedState::SCROLL_LOCK)
                    .bits(),
            ),
        })
    }

    pub const fn leds(self) -> KeyboardLedState {
        self.leds
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        KeyUsage, KeyboardFrame, KeyboardSuppression, ModifierState, PhysicalKeyboardState,
    };

    #[test]
    fn keyboard_report_uses_visible_press_order_and_modifier_bits() {
        let mut keyboard = PhysicalKeyboardState::new();
        let mut suppression = KeyboardSuppression::new();
        let mut frame = KeyboardFrame::new(ModifierState::LEFT_SHIFT | ModifierState::RIGHT_ALT);
        frame.push_key(KeyUsage(0x06)).unwrap();
        frame.push_key(KeyUsage(0x04)).unwrap();
        frame.push_key(KeyUsage(0x05)).unwrap();
        keyboard.apply_frame(&frame, &mut suppression).unwrap();

        let visible = keyboard.visible_against(&suppression);
        let build = BleKeyboard6KroReport::from_visible_state(&visible);

        assert!(!build.truncated);
        assert_eq!(
            build.report.as_bytes(),
            &[0b0100_0010, 0, 0x06, 0x04, 0x05, 0, 0, 0]
        );
    }

    #[test]
    fn keyboard_report_encodes_first_six_visible_keys_and_reports_truncation() {
        let mut keyboard = PhysicalKeyboardState::new();
        let mut suppression = KeyboardSuppression::new();
        let mut frame = KeyboardFrame::new(ModifierState::empty());
        for usage in 0x04..0x0b {
            frame.push_key(KeyUsage(usage)).unwrap();
        }
        keyboard.apply_frame(&frame, &mut suppression).unwrap();

        let visible = keyboard.visible_against(&suppression);
        let build = BleKeyboard6KroReport::from_visible_state(&visible);

        assert!(build.truncated);
        assert_eq!(
            build.report.as_bytes(),
            &[0, 0, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09]
        );
    }

    #[test]
    fn direct_physical_report_matches_visible_state_report() {
        let mut keyboard = PhysicalKeyboardState::new();
        let mut suppression = KeyboardSuppression::new();
        let mut frame = KeyboardFrame::new(ModifierState::LEFT_SHIFT);
        for usage in 0x04..0x0c {
            frame.push_key(KeyUsage(usage)).unwrap();
        }
        keyboard.apply_frame(&frame, &mut suppression).unwrap();
        suppression
            .suppress_visible_after(&keyboard, KEYBOARD_6KRO_KEY_CAPACITY)
            .unwrap();

        let visible = keyboard.visible_against(&suppression);
        assert_eq!(
            BleKeyboard6KroReport::from_physical_state(&keyboard, &suppression),
            BleKeyboard6KroReport::from_visible_state(&visible)
        );
    }

    #[test]
    fn keyboard_release_report_is_all_zeroes() {
        assert_eq!(
            BleKeyboard6KroReport::release().as_bytes(),
            &[0; KEYBOARD_REPORT_LEN]
        );
    }

    #[test]
    fn keyboard_report_can_be_built_from_raw_ble_bytes() {
        let report = BleKeyboard6KroReport::from_bytes([1, 2, 3, 4, 5, 6, 7, 8]);

        assert_eq!(report.as_bytes(), &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn keyboard_led_output_report_parses_standard_led_bits() {
        let report = BleKeyboardLedOutputReport::from_bytes(&[0b0000_0111]).unwrap();

        assert_eq!(
            report.leds(),
            KeyboardLedState::NUM_LOCK
                | KeyboardLedState::CAPS_LOCK
                | KeyboardLedState::SCROLL_LOCK
        );
    }

    #[test]
    fn keyboard_led_output_report_ignores_unsupported_led_bits() {
        let report = BleKeyboardLedOutputReport::from_bytes(&[0b1111_1000]).unwrap();

        assert_eq!(report.leds(), KeyboardLedState::empty());
    }

    #[test]
    fn keyboard_led_output_report_rejects_wrong_length() {
        assert_eq!(
            BleKeyboardLedOutputReport::from_bytes(&[]),
            Err(BleKeyboardOutputError::InvalidLength)
        );
        assert_eq!(
            BleKeyboardLedOutputReport::from_bytes(&[0, 0]),
            Err(BleKeyboardOutputError::InvalidLength)
        );
    }
}
