pub const FALLBACK_USB_VENDOR_ID: u16 = 0xCAFE;
pub const FALLBACK_USB_PRODUCT_ID: u16 = 0x4853;
pub const FALLBACK_USB_DEVICE_RELEASE: u16 = 0x0002;
pub const FALLBACK_USB_MANUFACTURER: &str = "HIDShift";
pub const FALLBACK_USB_PRODUCT: &str = "HIDShift Wired";

pub const FALLBACK_DEVICE_DESCRIPTOR: [u8; 18] = [
    18,
    0x01,
    0x00,
    0x02, // USB 2.0
    0,
    0,
    0,
    64,
    FALLBACK_USB_VENDOR_ID as u8,
    (FALLBACK_USB_VENDOR_ID >> 8) as u8,
    FALLBACK_USB_PRODUCT_ID as u8,
    (FALLBACK_USB_PRODUCT_ID >> 8) as u8,
    FALLBACK_USB_DEVICE_RELEASE as u8,
    (FALLBACK_USB_DEVICE_RELEASE >> 8) as u8,
    1,
    2,
    0,
    1,
];

pub const FALLBACK_CONFIGURATION_DESCRIPTOR: [u8; 91] = [
    9, 0x02, 91, 0, 3, 1, 0, 0x80, 50, // configuration
    9, 0x04, 0, 0, 2, 0x03, 0x01, 0x01, 0, // boot keyboard
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 65, 0, // keyboard HID
    7, 0x05, 0x81, 0x03, 8, 0, 1, // keyboard IN
    7, 0x05, 0x01, 0x03, 1, 0, 1, // keyboard OUT
    9, 0x04, 1, 0, 1, 0x03, 0x01, 0x02, 0, // boot mouse
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 55, 0, // mouse HID
    7, 0x05, 0x82, 0x03, 5, 0, 1, // mouse IN
    9, 0x04, 2, 0, 1, 0x03, 0, 0, 0, // consumer control
    9, 0x21, 0x11, 0x01, 0, 1, 0x22, 23, 0, // consumer HID
    7, 0x05, 0x83, 0x03, 2, 0, 1, // consumer IN
];

const LANGUAGES_STRING_DESCRIPTOR: [u8; 4] = [4, 0x03, 0x09, 0x04];
const MANUFACTURER_STRING_DESCRIPTOR: [u8; 18] = [
    18, 0x03, b'H', 0, b'I', 0, b'D', 0, b'S', 0, b'h', 0, b'i', 0, b'f', 0, b't', 0,
];
const PRODUCT_STRING_DESCRIPTOR: [u8; 30] = [
    30, 0x03, b'H', 0, b'I', 0, b'D', 0, b'S', 0, b'h', 0, b'i', 0, b'f', 0, b't', 0, b' ', 0,
    b'W', 0, b'i', 0, b'r', 0, b'e', 0, b'd', 0,
];

pub const KEYBOARD_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xa1, 0x01, // Collection (Application)
    0x05, 0x07, // Usage Page (Keyboard)
    0x19, 0xe0, // Usage Minimum (Left Control)
    0x29, 0xe7, // Usage Maximum (Right GUI)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x01, // Logical Maximum (1)
    0x75, 0x01, // Report Size (1)
    0x95, 0x08, // Report Count (8)
    0x81, 0x02, // Input (Data, Variable, Absolute)
    0x95, 0x01, // Report Count (1)
    0x75, 0x08, // Report Size (8)
    0x81, 0x01, // Input (Constant)
    0x95, 0x05, // Report Count (5)
    0x75, 0x01, // Report Size (1)
    0x05, 0x08, // Usage Page (LEDs)
    0x19, 0x01, // Usage Minimum (Num Lock)
    0x29, 0x05, // Usage Maximum (Kana)
    0x91, 0x02, // Output (Data, Variable, Absolute)
    0x95, 0x01, // Report Count (1)
    0x75, 0x03, // Report Size (3)
    0x91, 0x01, // Output (Constant)
    0x95, 0x06, // Report Count (6)
    0x75, 0x08, // Report Size (8)
    0x15, 0x00, // Logical Minimum (0)
    0x26, 0xff, 0x00, // Logical Maximum (255)
    0x05, 0x07, // Usage Page (Keyboard)
    0x19, 0x00, // Usage Minimum (0)
    0x2a, 0xff, 0x00, // Usage Maximum (255)
    0x81, 0x00, // Input (Data, Array, Absolute)
    0xc0, // End Collection
];

pub const MOUSE_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xa1, 0x01, // Collection (Application)
    0x09, 0x01, // Usage (Pointer)
    0xa1, 0x00, // Collection (Physical)
    0x05, 0x09, // Usage Page (Button)
    0x19, 0x01, // Usage Minimum (1)
    0x29, 0x08, // Usage Maximum (8)
    0x15, 0x00, // Logical Minimum (0)
    0x25, 0x01, // Logical Maximum (1)
    0x95, 0x08, // Report Count (8)
    0x75, 0x01, // Report Size (1)
    0x81, 0x02, // Input (Data, Variable, Absolute)
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x30, // Usage (X)
    0x09, 0x31, // Usage (Y)
    0x09, 0x38, // Usage (Wheel)
    0x15, 0x81, // Logical Minimum (-127)
    0x25, 0x7f, // Logical Maximum (127)
    0x75, 0x08, // Report Size (8)
    0x95, 0x03, // Report Count (3)
    0x81, 0x06, // Input (Data, Variable, Relative)
    0x05, 0x0c, // Usage Page (Consumer)
    0x0a, 0x38, 0x02, // Usage (AC Pan)
    0x95, 0x01, // Report Count (1)
    0x81, 0x06, // Input (Data, Variable, Relative)
    0xc0, // End Collection
    0xc0, // End Collection
];

pub const CONSUMER_REPORT_DESCRIPTOR: &[u8] = &[
    0x05, 0x0c, // Usage Page (Consumer)
    0x09, 0x01, // Usage (Consumer Control)
    0xa1, 0x01, // Collection (Application)
    0x15, 0x00, // Logical Minimum (0)
    0x26, 0xff, 0x03, // Logical Maximum (1023)
    0x19, 0x00, // Usage Minimum (0)
    0x2a, 0xff, 0x03, // Usage Maximum (1023)
    0x75, 0x10, // Report Size (16)
    0x95, 0x01, // Report Count (1)
    0x81, 0x00, // Input (Data, Array, Absolute)
    0xc0, // End Collection
];

pub fn build_fallback_mirror_image(
    out: &mut [u8],
) -> Result<usize, crate::mirror::MirrorImageEncodeError> {
    let strings = [
        crate::mirror::StringRecord {
            index: 0,
            lang_id: 0,
            descriptor: &LANGUAGES_STRING_DESCRIPTOR,
        },
        crate::mirror::StringRecord {
            index: 1,
            lang_id: 0x0409,
            descriptor: &MANUFACTURER_STRING_DESCRIPTOR,
        },
        crate::mirror::StringRecord {
            index: 2,
            lang_id: 0x0409,
            descriptor: &PRODUCT_STRING_DESCRIPTOR,
        },
    ];
    let reports = [
        crate::mirror::HidReportRecord {
            interface_number: 0,
            descriptor: KEYBOARD_REPORT_DESCRIPTOR,
        },
        crate::mirror::HidReportRecord {
            interface_number: 1,
            descriptor: MOUSE_REPORT_DESCRIPTOR,
        },
        crate::mirror::HidReportRecord {
            interface_number: 2,
            descriptor: CONSUMER_REPORT_DESCRIPTOR,
        },
    ];
    crate::mirror::serialize_mirror_image(
        crate::mirror::MirrorImageSource {
            flags: 0,
            device_descriptor: &FALLBACK_DEVICE_DESCRIPTOR,
            configuration_descriptor: &FALLBACK_CONFIGURATION_DESCRIPTOR,
            bos_descriptor: &[],
            strings: &strings,
            hid_reports: &reports,
        },
        out,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidreport::ReportDescriptor;

    #[test]
    fn every_builtin_descriptor_is_valid_hid() {
        for descriptor in [
            KEYBOARD_REPORT_DESCRIPTOR,
            MOUSE_REPORT_DESCRIPTOR,
            CONSUMER_REPORT_DESCRIPTOR,
        ] {
            assert!(ReportDescriptor::try_from(descriptor).is_ok());
        }
    }

    #[test]
    fn fallback_identity_is_not_a_serial_or_management_device() {
        assert_eq!(FALLBACK_USB_PRODUCT, "HIDShift Wired");
        assert_ne!(FALLBACK_USB_VENDOR_ID, 0);
        assert_ne!(FALLBACK_USB_PRODUCT_ID, 0);
    }

    #[test]
    fn fallback_uses_the_same_mirror_image_validator_and_endpoint_planner() {
        let mut bytes = [0; 1024];
        let length = build_fallback_mirror_image(&mut bytes).unwrap();
        let plan = crate::mirror::validate_mirror_image(&bytes[..length]).unwrap();

        assert_eq!(plan.interfaces.len(), 3);
        assert_eq!(plan.endpoints.len(), 4);
        assert_eq!(
            plan.endpoints
                .iter()
                .map(|endpoint| endpoint.address)
                .collect::<heapless::Vec<_, 4>>()
                .as_slice(),
            &[0x81, 0x01, 0x82, 0x83]
        );
    }
}
