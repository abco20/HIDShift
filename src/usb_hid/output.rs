use crate::input::KeyboardLedState;
use crate::usb_hid::report::USAGE_PAGE_LED;

pub const MAX_KEYBOARD_LED_OUTPUT_REPORT_LEN: usize = 8;

pub const LED_USAGE_NUM_LOCK: u16 = 0x01;
pub const LED_USAGE_CAPS_LOCK: u16 = 0x02;
pub const LED_USAGE_SCROLL_LOCK: u16 = 0x03;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardLedOutputError {
    ReportTooLong,
    MissingLedUsages,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardLedOutputReport {
    pub report_id: Option<u8>,
    pub byte_len: usize,
    pub num_lock_bit: Option<BitPos>,
    pub caps_lock_bit: Option<BitPos>,
    pub scroll_lock_bit: Option<BitPos>,
}

impl KeyboardLedOutputReport {
    pub const fn boot_keyboard() -> Self {
        Self {
            report_id: None,
            byte_len: 1,
            num_lock_bit: Some(BitPos::new(0, 0)),
            caps_lock_bit: Some(BitPos::new(0, 1)),
            scroll_lock_bit: Some(BitPos::new(0, 2)),
        }
    }

    pub fn from_report_descriptor(descriptor: &[u8]) -> Result<Self, KeyboardLedOutputError> {
        let mut parser = OutputReportParser::new();
        parser.parse(descriptor);
        parser.keyboard_led_report()
    }

    pub fn build(
        self,
        leds: KeyboardLedState,
    ) -> Result<KeyboardLedOutputBytes, KeyboardLedOutputError> {
        if self.byte_len > MAX_KEYBOARD_LED_OUTPUT_REPORT_LEN {
            return Err(KeyboardLedOutputError::ReportTooLong);
        }

        let mut bytes = KeyboardLedOutputBytes {
            len: self.byte_len,
            report_id: self.report_id,
            bytes: [0; MAX_KEYBOARD_LED_OUTPUT_REPORT_LEN],
        };

        let data_offset = if let Some(report_id) = self.report_id {
            bytes.bytes[0] = report_id;
            1
        } else {
            0
        };

        set_led_bit(
            &mut bytes.bytes,
            data_offset,
            self.num_lock_bit,
            leds.contains(KeyboardLedState::NUM_LOCK),
        );
        set_led_bit(
            &mut bytes.bytes,
            data_offset,
            self.caps_lock_bit,
            leds.contains(KeyboardLedState::CAPS_LOCK),
        );
        set_led_bit(
            &mut bytes.bytes,
            data_offset,
            self.scroll_lock_bit,
            leds.contains(KeyboardLedState::SCROLL_LOCK),
        );

        Ok(bytes)
    }
}

const MAX_OUTPUT_REPORTS: usize = 16;
const MAX_LOCAL_USAGES: usize = 16;
const MAX_GLOBAL_STACK_DEPTH: usize = 4;

#[derive(Clone, Copy)]
struct GlobalState {
    usage_page: u16,
    report_size: u8,
    report_count: u16,
    report_id: u8,
}

impl GlobalState {
    const EMPTY: Self = Self {
        usage_page: 0,
        report_size: 0,
        report_count: 0,
        report_id: 0,
    };
}

#[derive(Clone, Copy)]
struct LocalState {
    usages: [u16; MAX_LOCAL_USAGES],
    usage_count: usize,
    usage_min: u16,
    usage_max: u16,
    has_usage_range: bool,
}

impl LocalState {
    const EMPTY: Self = Self {
        usages: [0; MAX_LOCAL_USAGES],
        usage_count: 0,
        usage_min: 0,
        usage_max: 0,
        has_usage_range: false,
    };

    fn usage_at(self, index: usize) -> Option<u16> {
        if self.has_usage_range {
            let usage = self.usage_min.checked_add(index as u16)?;
            return (usage <= self.usage_max).then_some(usage);
        }
        (index < self.usage_count).then_some(self.usages[index])
    }
}

#[derive(Clone, Copy)]
struct OutputReportState {
    report_id: u8,
    bit_len: usize,
    num_lock_bit: Option<BitPos>,
    caps_lock_bit: Option<BitPos>,
    scroll_lock_bit: Option<BitPos>,
}

impl OutputReportState {
    const EMPTY: Self = Self {
        report_id: 0,
        bit_len: 0,
        num_lock_bit: None,
        caps_lock_bit: None,
        scroll_lock_bit: None,
    };

    const fn has_keyboard_leds(self) -> bool {
        self.num_lock_bit.is_some()
            || self.caps_lock_bit.is_some()
            || self.scroll_lock_bit.is_some()
    }
}

struct OutputReportParser {
    global: GlobalState,
    local: LocalState,
    global_stack: [GlobalState; MAX_GLOBAL_STACK_DEPTH],
    global_stack_len: usize,
    reports: [OutputReportState; MAX_OUTPUT_REPORTS],
    report_count: usize,
    has_report_ids: bool,
    collection_depth: usize,
    keyboard_application_depth: Option<usize>,
}

impl OutputReportParser {
    fn new() -> Self {
        Self {
            global: GlobalState::EMPTY,
            local: LocalState::EMPTY,
            global_stack: [GlobalState::EMPTY; MAX_GLOBAL_STACK_DEPTH],
            global_stack_len: 0,
            reports: [OutputReportState::EMPTY; MAX_OUTPUT_REPORTS],
            report_count: 0,
            has_report_ids: false,
            collection_depth: 0,
            keyboard_application_depth: None,
        }
    }

    fn parse(&mut self, descriptor: &[u8]) {
        let mut offset = 0usize;
        while offset < descriptor.len() {
            let prefix = descriptor[offset];
            offset += 1;
            if prefix == 0xfe {
                if offset + 2 > descriptor.len() {
                    break;
                }
                let len = descriptor[offset] as usize;
                offset += 2;
                let Some(next) = offset.checked_add(len) else {
                    break;
                };
                if next > descriptor.len() {
                    break;
                }
                offset = next;
                continue;
            }

            let size = match prefix & 0x03 {
                3 => 4,
                size => size as usize,
            };
            if offset + size > descriptor.len() {
                break;
            }
            let mut data = 0u32;
            for (index, byte) in descriptor[offset..offset + size].iter().enumerate() {
                data |= (*byte as u32) << (index * 8);
            }
            offset += size;

            let item_type = (prefix >> 2) & 0x03;
            let tag = prefix >> 4;
            match (item_type, tag) {
                (1, 0) => self.global.usage_page = data as u16,
                (1, 7) => self.global.report_size = data as u8,
                (1, 8) => {
                    self.global.report_id = data as u8;
                    self.has_report_ids = true;
                }
                (1, 9) => self.global.report_count = data as u16,
                (1, 10) if self.global_stack_len < MAX_GLOBAL_STACK_DEPTH => {
                    self.global_stack[self.global_stack_len] = self.global;
                    self.global_stack_len += 1;
                }
                (1, 11) if self.global_stack_len > 0 => {
                    self.global_stack_len -= 1;
                    self.global = self.global_stack[self.global_stack_len];
                }
                (2, 0) if self.local.usage_count < MAX_LOCAL_USAGES => {
                    self.local.usages[self.local.usage_count] = data as u16;
                    self.local.usage_count += 1;
                }
                (2, 1) => {
                    self.local.usage_min = data as u16;
                    self.local.has_usage_range = true;
                }
                (2, 2) => {
                    self.local.usage_max = data as u16;
                    self.local.has_usage_range = true;
                }
                (0, 9) => self.parse_output(data as u8),
                (0, 10) => self.begin_collection(data as u8),
                (0, 12) => self.end_collection(),
                (0, 8) | (0, 11) => {
                    self.local = LocalState::EMPTY;
                }
                _ => {}
            }
        }
    }

    fn parse_output(&mut self, flags: u8) {
        let report_size = self.global.report_size as usize;
        let report_count = self.global.report_count as usize;
        let Some(report_index) = self.report_index(self.global.report_id) else {
            self.local = LocalState::EMPTY;
            return;
        };
        let bit_offset = self.reports[report_index].bit_len;
        self.reports[report_index].bit_len =
            bit_offset.saturating_add(report_size.saturating_mul(report_count));

        let is_constant = flags & 0x01 != 0;
        let is_variable = flags & 0x02 != 0;
        if !is_constant
            && is_variable
            && report_size == 1
            && self.global.usage_page == USAGE_PAGE_LED
            && self.keyboard_application_depth.is_some()
        {
            for index in 0..report_count {
                let Some(usage) = self.local.usage_at(index) else {
                    continue;
                };
                let bit = Some(BitPos::from_bit_offset(bit_offset + index));
                match usage {
                    LED_USAGE_NUM_LOCK => self.reports[report_index].num_lock_bit = bit,
                    LED_USAGE_CAPS_LOCK => self.reports[report_index].caps_lock_bit = bit,
                    LED_USAGE_SCROLL_LOCK => self.reports[report_index].scroll_lock_bit = bit,
                    _ => {}
                }
            }
        }
        self.local = LocalState::EMPTY;
    }

    fn begin_collection(&mut self, collection_type: u8) {
        self.collection_depth = self.collection_depth.saturating_add(1);
        if collection_type == 0x01 {
            let usage = self.local.usage_at(0);
            if self.global.usage_page == 0x01 && matches!(usage, Some(0x06) | Some(0x07)) {
                self.keyboard_application_depth = Some(self.collection_depth);
            }
        }
        self.local = LocalState::EMPTY;
    }

    fn end_collection(&mut self) {
        if self.keyboard_application_depth == Some(self.collection_depth) {
            self.keyboard_application_depth = None;
        }
        self.collection_depth = self.collection_depth.saturating_sub(1);
        self.local = LocalState::EMPTY;
    }

    fn report_index(&mut self, report_id: u8) -> Option<usize> {
        if let Some(index) = self.reports[..self.report_count]
            .iter()
            .position(|report| report.report_id == report_id)
        {
            return Some(index);
        }
        if self.report_count == MAX_OUTPUT_REPORTS {
            return None;
        }
        let index = self.report_count;
        self.report_count += 1;
        self.reports[index].report_id = report_id;
        Some(index)
    }

    fn keyboard_led_report(self) -> Result<KeyboardLedOutputReport, KeyboardLedOutputError> {
        let Some(report) = self.reports[..self.report_count]
            .iter()
            .copied()
            .find(|report| report.has_keyboard_leds())
        else {
            return Err(KeyboardLedOutputError::MissingLedUsages);
        };
        let payload_len = report.bit_len.div_ceil(8);
        let byte_len = payload_len + usize::from(self.has_report_ids);
        if byte_len > MAX_KEYBOARD_LED_OUTPUT_REPORT_LEN {
            return Err(KeyboardLedOutputError::ReportTooLong);
        }

        Ok(KeyboardLedOutputReport {
            report_id: self.has_report_ids.then_some(report.report_id),
            byte_len,
            num_lock_bit: report.num_lock_bit,
            caps_lock_bit: report.caps_lock_bit,
            scroll_lock_bit: report.scroll_lock_bit,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyboardLedOutputBytes {
    len: usize,
    report_id: Option<u8>,
    bytes: [u8; MAX_KEYBOARD_LED_OUTPUT_REPORT_LEN],
}

impl KeyboardLedOutputBytes {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    pub fn control_set_report(&self) -> (u8, &[u8]) {
        match self.report_id {
            Some(report_id) => (report_id, &self.bytes[..self.len]),
            None => (0, &self.bytes[..self.len]),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BitPos {
    pub byte_index: usize,
    pub bit_index: u8,
}

impl BitPos {
    pub const fn new(byte_index: usize, bit_index: u8) -> Self {
        Self {
            byte_index,
            bit_index,
        }
    }

    pub const fn from_bit_offset(bit_offset: usize) -> Self {
        Self {
            byte_index: bit_offset / 8,
            bit_index: (bit_offset % 8) as u8,
        }
    }
}

fn set_led_bit(
    bytes: &mut [u8; MAX_KEYBOARD_LED_OUTPUT_REPORT_LEN],
    data_offset: usize,
    bit_pos: Option<BitPos>,
    enabled: bool,
) {
    let Some(bit_pos) = bit_pos else {
        return;
    };
    let byte_index = data_offset + bit_pos.byte_index;
    if enabled {
        bytes[byte_index] |= 1 << bit_pos.bit_index;
    } else {
        bytes[byte_index] &= !(1 << bit_pos.bit_index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_keyboard_led_output_uses_expected_byte_layout() {
        let report = KeyboardLedOutputReport::boot_keyboard();

        let bytes = report
            .build(KeyboardLedState::NUM_LOCK | KeyboardLedState::SCROLL_LOCK)
            .unwrap();

        assert_eq!(bytes.as_slice(), &[0b0000_0101]);
    }

    #[test]
    fn descriptor_led_output_uses_declared_bit_positions() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, // Keyboard application.
            0x75, 0x01, 0x95, 0x01, 0x91, 0x01, // One constant output bit.
            0x05, 0x08, 0x19, 0x01, 0x29, 0x03, 0x95, 0x03, 0x91, 0x02, 0xc0,
        ];

        let report = KeyboardLedOutputReport::from_report_descriptor(&descriptor).unwrap();
        let bytes = report.build(KeyboardLedState::CAPS_LOCK).unwrap();

        assert_eq!(
            report,
            KeyboardLedOutputReport {
                report_id: None,
                byte_len: 1,
                num_lock_bit: Some(BitPos::new(0, 1)),
                caps_lock_bit: Some(BitPos::new(0, 2)),
                scroll_lock_bit: Some(BitPos::new(0, 3)),
            }
        );
        assert_eq!(bytes.as_slice(), &[0b0000_0100]);
    }

    #[test]
    fn descriptor_led_output_with_report_id_prefixes_report_id_byte() {
        let descriptor = [
            0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, // Keyboard application.
            0x85, 0x04, 0x05, 0x08, 0x19, 0x01, 0x29, 0x03, 0x75, 0x01, 0x95, 0x03, 0x91, 0x02,
            0xc0,
        ];

        let report = KeyboardLedOutputReport::from_report_descriptor(&descriptor).unwrap();
        let bytes = report
            .build(KeyboardLedState::NUM_LOCK | KeyboardLedState::CAPS_LOCK)
            .unwrap();

        assert_eq!(bytes.as_slice(), &[4, 0b0000_0011]);
        assert_eq!(bytes.control_set_report().0, 4);
        assert_eq!(bytes.control_set_report().1, &[4, 0b0000_0011]);
    }

    #[test]
    fn boot_keyboard_control_set_report_uses_report_id_zero() {
        let bytes = KeyboardLedOutputReport::boot_keyboard()
            .build(KeyboardLedState::CAPS_LOCK)
            .unwrap();

        assert_eq!(bytes.control_set_report().0, 0);
        assert_eq!(bytes.control_set_report().1, &[0b0000_0010]);
    }

    #[test]
    fn descriptor_without_standard_led_usages_is_rejected() {
        assert_eq!(
            KeyboardLedOutputReport::from_report_descriptor(&[]),
            Err(KeyboardLedOutputError::MissingLedUsages)
        );
    }

    #[test]
    fn input_led_usages_are_not_treated_as_keyboard_led_output() {
        let descriptor = [
            0x05, 0x08, // Usage Page (LED)
            0x19, 0x01, // Usage Minimum (Num Lock)
            0x29, 0x03, // Usage Maximum (Scroll Lock)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x01, // Logical Maximum (1)
            0x75, 0x01, // Report Size (1)
            0x95, 0x03, // Report Count (3)
            0x81, 0x02, // Input (Data, Variable, Absolute)
        ];

        assert_eq!(
            KeyboardLedOutputReport::from_report_descriptor(&descriptor),
            Err(KeyboardLedOutputError::MissingLedUsages)
        );
    }

    #[test]
    fn mouse_application_led_output_is_not_treated_as_keyboard_lock_leds() {
        let descriptor = [
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x02, // Usage (Mouse)
            0xa1, 0x01, // Collection (Application)
            0x85, 0x04, // Report ID (4)
            0x05, 0x08, // Usage Page (LED)
            0x19, 0x01, // Usage Minimum (Num Lock)
            0x29, 0x03, // Usage Maximum (Scroll Lock)
            0x75, 0x01, // Report Size (1)
            0x95, 0x03, // Report Count (3)
            0x91, 0x02, // Output (Data, Variable, Absolute)
            0xc0, // End Collection
        ];

        assert_eq!(
            KeyboardLedOutputReport::from_report_descriptor(&descriptor),
            Err(KeyboardLedOutputError::MissingLedUsages)
        );
    }

    #[test]
    fn output_parser_preserves_report_id_and_full_output_report_length() {
        let descriptor = [
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x06, // Usage (Keyboard)
            0xa1, 0x01, // Collection (Application)
            0x85, 0x04, // Report ID (4)
            0x05, 0x08, // Usage Page (LED)
            0x19, 0x01, // Usage Minimum (Num Lock)
            0x29, 0x03, // Usage Maximum (Scroll Lock)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x01, // Logical Maximum (1)
            0x75, 0x01, // Report Size (1)
            0x95, 0x03, // Report Count (3)
            0x91, 0x02, // Output (Data, Variable, Absolute)
            0x75, 0x01, // Report Size (1)
            0x95, 0x05, // Report Count (5)
            0x91, 0x01, // Output (Constant)
            0xc0, // End Collection
        ];

        let report = KeyboardLedOutputReport::from_report_descriptor(&descriptor).unwrap();

        assert_eq!(
            report,
            KeyboardLedOutputReport {
                report_id: Some(4),
                byte_len: 2,
                num_lock_bit: Some(BitPos::new(0, 0)),
                caps_lock_bit: Some(BitPos::new(0, 1)),
                scroll_lock_bit: Some(BitPos::new(0, 2)),
            }
        );
        assert_eq!(
            report
                .build(KeyboardLedState::CAPS_LOCK)
                .unwrap()
                .as_slice(),
            &[4, 0b0000_0010]
        );
    }
}
