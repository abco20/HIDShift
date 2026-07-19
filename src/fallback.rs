pub const FALLBACK_USB_VENDOR_ID: u16 = 0xCAFE;
pub const FALLBACK_USB_PRODUCT_ID: u16 = 0x4853;
pub const FALLBACK_USB_DEVICE_RELEASE: u16 = 0x0002;
pub const FALLBACK_USB_MANUFACTURER: &str = "HIDShift";
pub const FALLBACK_USB_PRODUCT: &str = "HIDShift Wired";

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
}
