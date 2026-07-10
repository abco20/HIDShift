use super::{BleHidReport, ReportKind};
use crate::reports::{
    CONSUMER_REPORT_LEN, KEYBOARD_REPORT_ID, KEYBOARD_REPORT_LEN, MOUSE_REPORT_LEN, ble_keyboard,
};

pub const INPUT_REPORT_TYPE: u8 = 1;
pub const OUTPUT_REPORT_TYPE: u8 = 2;
pub const FEATURE_REPORT_TYPE: u8 = 3;
pub const HID_INFORMATION: [u8; 4] = [
    0x11, 0x01, // HID version 1.11
    0x00, // country code: not localized
    0x03, // normally connectable + remote wake
];

pub const BLE_HID_INPUT_REPORT_MAX_LEN: usize = KEYBOARD_REPORT_LEN;
pub const BLE_HID_NOTIFY_MAX_LEN: usize = BLE_HID_INPUT_REPORT_MAX_LEN;
pub const BLE_HID_NOTIFICATIONS_PER_REPORT_MAX: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleHidInputReport {
    len: usize,
    bytes: [u8; BLE_HID_INPUT_REPORT_MAX_LEN],
}

impl BleHidInputReport {
    pub fn from_report(report: BleHidReport) -> Self {
        let mut bytes = [0; BLE_HID_INPUT_REPORT_MAX_LEN];
        let len = match report {
            BleHidReport::Keyboard(report) => {
                bytes[..KEYBOARD_REPORT_LEN].copy_from_slice(report.as_bytes());
                KEYBOARD_REPORT_LEN
            }
            BleHidReport::Mouse(report) => {
                bytes[..MOUSE_REPORT_LEN].copy_from_slice(report.as_bytes());
                MOUSE_REPORT_LEN
            }
            BleHidReport::Consumer(report) => {
                bytes[..CONSUMER_REPORT_LEN].copy_from_slice(report.as_bytes());
                CONSUMER_REPORT_LEN
            }
        };

        Self { len, bytes }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleHidCharacteristic {
    KeyboardInputReport,
    MouseInputReport,
    ConsumerInputReport,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleHidNotification {
    pub characteristic: BleHidCharacteristic,
    pub(crate) len: usize,
    pub(crate) bytes: [u8; BLE_HID_NOTIFY_MAX_LEN],
}

impl BleHidNotification {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleHidNotificationError {
    Capacity,
}

pub fn notifications_for_input_report<const N: usize>(
    report: BleHidReport,
    out: &mut heapless::Vec<BleHidNotification, N>,
) -> Result<(), BleHidNotificationError> {
    out.clear();

    let input = BleHidInputReport::from_report(report);
    push_notification(
        out,
        BleHidNotification {
            characteristic: input_report_characteristic(report.kind()),
            len: input.as_slice().len(),
            bytes: padded_notification_bytes(input.as_slice()),
        },
    )?;

    Ok(())
}

pub const fn input_report_characteristic(kind: ReportKind) -> BleHidCharacteristic {
    match kind {
        ReportKind::Keyboard => BleHidCharacteristic::KeyboardInputReport,
        ReportKind::Mouse => BleHidCharacteristic::MouseInputReport,
        ReportKind::Consumer => BleHidCharacteristic::ConsumerInputReport,
        ReportKind::KeyboardOutput => BleHidCharacteristic::KeyboardInputReport,
    }
}

pub const fn report_id(kind: ReportKind) -> u8 {
    match kind {
        ReportKind::Keyboard | ReportKind::KeyboardOutput => KEYBOARD_REPORT_ID,
        ReportKind::Mouse | ReportKind::Consumer => 0,
    }
}

fn push_notification<const N: usize>(
    out: &mut heapless::Vec<BleHidNotification, N>,
    notification: BleHidNotification,
) -> Result<(), BleHidNotificationError> {
    out.push(notification)
        .map_err(|_| BleHidNotificationError::Capacity)
}

fn padded_notification_bytes(bytes: &[u8]) -> [u8; BLE_HID_NOTIFY_MAX_LEN] {
    let mut out = [0; BLE_HID_NOTIFY_MAX_LEN];
    out[..bytes.len()].copy_from_slice(bytes);
    out
}

pub const fn report_type(kind: ReportKind) -> u8 {
    match kind {
        ReportKind::Keyboard | ReportKind::Mouse | ReportKind::Consumer => INPUT_REPORT_TYPE,
        ReportKind::KeyboardOutput => OUTPUT_REPORT_TYPE,
    }
}

pub const V1_KEYBOARD_REPORT_MAP: &[u8] = &[
    // A shared ID lets BlueZ route UHID output back to this Report characteristic.
    0x05,
    0x01, // Usage Page (Generic Desktop)
    0x09,
    0x06, // Usage (Keyboard)
    0xa1,
    0x01, // Collection (Application)
    0x85,
    KEYBOARD_REPORT_ID, // Report ID (1)
    0x05,
    0x07, // Usage Page (Keyboard/Keypad)
    0x19,
    0xe0, // Usage Minimum (Keyboard LeftControl)
    0x29,
    0xe7, // Usage Maximum (Keyboard Right GUI)
    0x15,
    0x00, // Logical Minimum (0)
    0x25,
    0x01, // Logical Maximum (1)
    0x75,
    0x01, // Report Size (1)
    0x95,
    0x08, // Report Count (8)
    0x81,
    0x02, // Input (Data,Var,Abs)
    0x95,
    0x01, // Report Count (1)
    0x75,
    0x08, // Report Size (8)
    0x81,
    0x01, // Input (Const,Array,Abs)
    0x95,
    0x05, // Report Count (5)
    0x75,
    0x01, // Report Size (1)
    0x05,
    0x08, // Usage Page (LEDs)
    0x19,
    0x01, // Usage Minimum (Num Lock)
    0x29,
    0x05, // Usage Maximum (Kana)
    0x91,
    0x02, // Output (Data,Var,Abs)
    0x95,
    0x01, // Report Count (1)
    0x75,
    0x03, // Report Size (3)
    0x91,
    0x01, // Output (Const,Array,Abs)
    0x95,
    ble_keyboard::KEYBOARD_6KRO_KEY_CAPACITY as u8, // Report Count (6)
    0x75,
    0x08, // Report Size (8)
    0x15,
    0x00, // Logical Minimum (0)
    0x25,
    0x65, // Logical Maximum (101)
    0x05,
    0x07, // Usage Page (Keyboard/Keypad)
    0x19,
    0x00, // Usage Minimum (Reserved)
    0x29,
    0x65, // Usage Maximum (Keyboard Application)
    0x81,
    0x00, // Input (Data,Array,Abs)
    0xc0, // End Collection
];

pub const V1_MOUSE_REPORT_MAP: &[u8] = &[
    // One unnumbered mouse input report.
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x01, // Usage (Pointer)
    0xa1, 0x00, // Collection (Physical)
    0x05, 0x09, // Usage Page (Buttons)
    0x19, 0x01, // Usage Minimum (Button 1)
    0x29, 0x05, // Usage Maximum (Button 5)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x01, // Logical Maximum (1)
    0x75, 0x01, // Report Size (1)
    0x95, 0x05, // Report Count (5)
    0x81, 0x02, // Input (Data,Var,Abs)
    0x75, 0x03, // Report Size (3)
    0x95, 0x01, // Report Count (1)
    0x81, 0x01, // Input (Const,Array,Abs)
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x30, // Usage (X)
    0x09, 0x31, // Usage (Y)
    0x09, 0x38, // Usage (Wheel)
    0x15, 0x81, // Logical Minimum (-127)
    0x25, 0x7f, // Logical Maximum (127)
    0x75, 0x08, // Report Size (8)
    0x95, 0x03, // Report Count (3)
    0x81, 0x06, // Input (Data,Var,Rel)
    0x05, 0x0c, // Usage Page (Consumer)
    0x0a, 0x38, 0x02, // Usage (AC Pan)
    0x95, 0x01, // Report Count (1)
    0x81, 0x06, // Input (Data,Var,Rel)
    0xc0, // End Collection
    0xc0, // End Collection
];

pub const V1_CONSUMER_REPORT_MAP: &[u8] = &[
    // One unnumbered consumer-control input report.
    0x05, 0x0c, // Usage Page (Consumer)
    0x09, 0x01, // Usage (Consumer Control)
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, // Logical Minimum (0)
    0x26, 0xff, 0x03, // Logical Maximum (0x03ff)
    0x19, 0x00, // Usage Minimum (Unassigned)
    0x2a, 0xff, 0x03, // Usage Maximum (0x03ff)
    0x75, 0x10, // Report Size (16)
    0x95, 0x01, // Report Count (1)
    0x81, 0x00, // Input (Data,Array,Abs)
    0xc0, // End Collection
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reports::{BleConsumerReport, BleKeyboard6KroReport, BleMouseReport};
    use hidreport::{Field, Report, ReportDescriptor, ReportId, UsageId, UsagePage};

    #[test]
    fn keyboard_uses_numbered_report_references_for_bluez_output_routing() {
        assert_eq!(report_id(ReportKind::Keyboard), KEYBOARD_REPORT_ID);
        assert_eq!(report_id(ReportKind::Mouse), 0);
        assert_eq!(report_id(ReportKind::Consumer), 0);
        assert_eq!(report_id(ReportKind::KeyboardOutput), KEYBOARD_REPORT_ID);
        assert_eq!(report_type(ReportKind::KeyboardOutput), OUTPUT_REPORT_TYPE);
    }

    #[test]
    fn gatt_report_characteristic_payloads_exclude_report_id() {
        assert_eq!(
            BleHidInputReport::from_report(
                BleHidReport::Keyboard(BleKeyboard6KroReport::release())
            )
            .as_slice(),
            &[0; KEYBOARD_REPORT_LEN]
        );
        assert_eq!(
            BleHidInputReport::from_report(BleHidReport::Mouse(BleMouseReport::release_buttons()))
                .as_slice(),
            &[0; MOUSE_REPORT_LEN]
        );
        assert_eq!(
            BleHidInputReport::from_report(BleHidReport::Consumer(BleConsumerReport::release()))
                .as_slice(),
            &[0; CONSUMER_REPORT_LEN]
        );
    }

    #[test]
    fn keyboard_input_report_notifies_only_its_report_characteristic() {
        let mut notifications =
            heapless::Vec::<BleHidNotification, BLE_HID_NOTIFICATIONS_PER_REPORT_MAX>::new();

        notifications_for_input_report(
            BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
            &mut notifications,
        )
        .unwrap();

        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].characteristic,
            BleHidCharacteristic::KeyboardInputReport
        );
        assert_eq!(notifications[0].as_slice(), &[0; KEYBOARD_REPORT_LEN]);
    }

    #[test]
    fn mouse_and_consumer_reports_notify_only_their_report_characteristic() {
        let mut notifications =
            heapless::Vec::<BleHidNotification, BLE_HID_NOTIFICATIONS_PER_REPORT_MAX>::new();

        notifications_for_input_report(
            BleHidReport::Mouse(BleMouseReport::release_buttons()),
            &mut notifications,
        )
        .unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].characteristic,
            BleHidCharacteristic::MouseInputReport
        );
        assert_eq!(notifications[0].as_slice(), &[0; MOUSE_REPORT_LEN]);

        notifications_for_input_report(
            BleHidReport::Consumer(BleConsumerReport::release()),
            &mut notifications,
        )
        .unwrap();
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].characteristic,
            BleHidCharacteristic::ConsumerInputReport
        );
        assert_eq!(notifications[0].as_slice(), &[0; CONSUMER_REPORT_LEN]);
    }

    #[test]
    fn notification_builder_reports_capacity_error() {
        let mut notifications = heapless::Vec::<BleHidNotification, 0>::new();

        assert_eq!(
            notifications_for_input_report(
                BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                &mut notifications,
            ),
            Err(BleHidNotificationError::Capacity)
        );
    }

    #[test]
    fn only_keyboard_report_map_declares_a_report_id() {
        assert!(has_report_id(V1_KEYBOARD_REPORT_MAP));
        assert!(!has_report_id(V1_MOUSE_REPORT_MAP));
        assert!(!has_report_id(V1_CONSUMER_REPORT_MAP));
    }

    #[test]
    fn split_report_maps_match_rust_report_structs_with_hidreport() {
        let keyboard_descriptor = ReportDescriptor::try_from(V1_KEYBOARD_REPORT_MAP).unwrap();
        let mouse_descriptor = ReportDescriptor::try_from(V1_MOUSE_REPORT_MAP).unwrap();
        let consumer_descriptor = ReportDescriptor::try_from(V1_CONSUMER_REPORT_MAP).unwrap();

        let keyboard = &keyboard_descriptor.input_reports()[0];
        let keyboard_output = &keyboard_descriptor.output_reports()[0];
        let mouse = &mouse_descriptor.input_reports()[0];
        let consumer = &consumer_descriptor.input_reports()[0];

        assert_eq!(keyboard_descriptor.input_reports().len(), 1);
        assert_eq!(keyboard_descriptor.output_reports().len(), 1);
        assert_eq!(mouse_descriptor.input_reports().len(), 1);
        assert_eq!(consumer_descriptor.input_reports().len(), 1);
        assert_eq!(
            keyboard.report_id(),
            &Some(ReportId::from(KEYBOARD_REPORT_ID))
        );
        assert_eq!(
            keyboard_output.report_id(),
            &Some(ReportId::from(KEYBOARD_REPORT_ID))
        );
        assert_eq!(mouse.report_id(), &None);
        assert_eq!(consumer.report_id(), &None);
        assert_eq!(keyboard.size_in_bytes(), KEYBOARD_REPORT_LEN + 1);
        assert_eq!(mouse.size_in_bytes(), MOUSE_REPORT_LEN);
        assert_eq!(consumer.size_in_bytes(), CONSUMER_REPORT_LEN);
        assert_eq!(keyboard_output.size_in_bytes(), 2);

        let led_usages = keyboard_output
            .fields()
            .iter()
            .filter_map(|field| match field {
                Field::Variable(field) => Some((field.usage.usage_page, field.usage.usage_id)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(led_usages.contains(&(UsagePage::from(0x08), UsageId::from(0x01))));
        assert!(led_usages.contains(&(UsagePage::from(0x08), UsageId::from(0x02))));
        assert!(led_usages.contains(&(UsagePage::from(0x08), UsageId::from(0x03))));

        assert!(mouse.fields().iter().any(|field| matches!(
            field,
            Field::Variable(field)
                if field.usage.usage_page == UsagePage::from(0x01)
                    && field.usage.usage_id == UsageId::from(0x38)
        )));
        assert!(mouse.fields().iter().any(|field| matches!(
            field,
            Field::Variable(field)
                if field.usage.usage_page == UsagePage::from(0x0c)
                    && field.usage.usage_id == UsageId::from(0x0238)
        )));
    }

    #[test]
    fn split_report_maps_expose_keyboard_leds_and_mouse_pan() {
        let keyboard_descriptor = ReportDescriptor::try_from(V1_KEYBOARD_REPORT_MAP).unwrap();
        let mouse_descriptor = ReportDescriptor::try_from(V1_MOUSE_REPORT_MAP).unwrap();
        let output = &keyboard_descriptor.output_reports()[0];
        let mouse = &mouse_descriptor.input_reports()[0];

        let led_fields = output
            .fields()
            .iter()
            .filter(|field| matches!(field, Field::Variable(_)))
            .count();
        assert!(led_fields >= 5);

        let mouse_has_pan = mouse.fields().iter().any(|field| match field {
            Field::Variable(field) => {
                field.usage.usage_page == UsagePage::from(0x0c)
                    && field.usage.usage_id == UsageId::from(0x0238)
            }
            _ => false,
        });
        assert!(mouse_has_pan);
    }

    fn has_report_id(map: &[u8]) -> bool {
        map.contains(&0x85)
    }
}
