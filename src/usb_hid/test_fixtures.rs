use crate::usb_hid::report::{
    FIELD_FLAG_VARIABLE, HidReportDescriptor, ReportField, USAGE_PAGE_BUTTON, USAGE_PAGE_CONSUMER,
    USAGE_PAGE_GENERIC_DESKTOP, USAGE_PAGE_KEYBOARD, USAGE_PAGE_LED, USAGE_WHEEL, USAGE_X, USAGE_Y,
};

pub const USAGE_AC_PAN: u16 = 0x0238;

pub fn composite_report_id_descriptor() -> HidReportDescriptor<8> {
    let mut descriptor = HidReportDescriptor::new(true);
    descriptor
        .push(ReportField {
            report_id: 1,
            usage_page: USAGE_PAGE_KEYBOARD,
            usage_min: 0xe0,
            usage_max: 0xe7,
            bit_offset: 0,
            bit_size: 1,
            count: 8,
            flags: FIELD_FLAG_VARIABLE,
            logical_min: 0,
            logical_max: 1,
        })
        .unwrap();
    descriptor
        .push(ReportField {
            report_id: 1,
            usage_page: USAGE_PAGE_KEYBOARD,
            usage_min: 0,
            usage_max: 0x65,
            bit_offset: 16,
            bit_size: 8,
            count: 6,
            flags: 0,
            logical_min: 0,
            logical_max: 0x65,
        })
        .unwrap();
    descriptor
        .push(ReportField {
            report_id: 1,
            usage_page: USAGE_PAGE_LED,
            usage_min: 1,
            usage_max: 5,
            bit_offset: 0,
            bit_size: 1,
            count: 5,
            flags: FIELD_FLAG_VARIABLE,
            logical_min: 0,
            logical_max: 1,
        })
        .unwrap();
    descriptor
        .push(ReportField {
            report_id: 2,
            usage_page: USAGE_PAGE_BUTTON,
            usage_min: 1,
            usage_max: 5,
            bit_offset: 0,
            bit_size: 1,
            count: 5,
            flags: FIELD_FLAG_VARIABLE,
            logical_min: 0,
            logical_max: 1,
        })
        .unwrap();
    descriptor
        .push(ReportField {
            report_id: 2,
            usage_page: USAGE_PAGE_GENERIC_DESKTOP,
            usage_min: USAGE_X,
            usage_max: USAGE_Y,
            bit_offset: 8,
            bit_size: 8,
            count: 2,
            flags: FIELD_FLAG_VARIABLE,
            logical_min: -127,
            logical_max: 127,
        })
        .unwrap();
    descriptor
        .push(ReportField {
            report_id: 2,
            usage_page: USAGE_PAGE_GENERIC_DESKTOP,
            usage_min: USAGE_WHEEL,
            usage_max: USAGE_WHEEL,
            bit_offset: 24,
            bit_size: 8,
            count: 1,
            flags: FIELD_FLAG_VARIABLE,
            logical_min: -127,
            logical_max: 127,
        })
        .unwrap();
    descriptor
        .push(ReportField {
            report_id: 2,
            usage_page: USAGE_PAGE_CONSUMER,
            usage_min: USAGE_AC_PAN,
            usage_max: USAGE_AC_PAN,
            bit_offset: 32,
            bit_size: 8,
            count: 1,
            flags: FIELD_FLAG_VARIABLE,
            logical_min: -127,
            logical_max: 127,
        })
        .unwrap();
    descriptor
        .push(ReportField {
            report_id: 3,
            usage_page: USAGE_PAGE_CONSUMER,
            usage_min: 0,
            usage_max: 0x03ff,
            bit_offset: 0,
            bit_size: 16,
            count: 1,
            flags: 0,
            logical_min: 0,
            logical_max: 0x03ff,
        })
        .unwrap();
    descriptor
}

pub fn consumer_array_descriptor() -> HidReportDescriptor<1> {
    let mut descriptor = HidReportDescriptor::new(true);
    descriptor
        .push(ReportField {
            report_id: 3,
            usage_page: USAGE_PAGE_CONSUMER,
            usage_min: 0,
            usage_max: 0x03ff,
            bit_offset: 0,
            bit_size: 16,
            count: 1,
            flags: 0,
            logical_min: 0,
            logical_max: 0x03ff,
        })
        .unwrap();
    descriptor
}

pub const fn keyboard_report(modifiers: u8, key0: u8, key1: u8) -> [u8; 9] {
    [1, modifiers, 0, key0, key1, 0, 0, 0, 0]
}

pub const fn mouse_report(buttons: u8, x: i8, y: i8, wheel: i8, pan: i8) -> [u8; 6] {
    [2, buttons, x as u8, y as u8, wheel as u8, pan as u8]
}

pub const fn consumer_report(usage: u16) -> [u8; 3] {
    [3, (usage & 0xff) as u8, (usage >> 8) as u8]
}
