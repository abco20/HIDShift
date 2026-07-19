use crate::ids::{DeviceId, InterfaceId};
use crate::runtime::message::RuntimeInputMessage;
use crate::usb_hid::frame::UsbInputFrameError;
use crate::usb_hid::output::{KeyboardLedOutputError, KeyboardLedOutputReport};
use crate::usb_hid::report::{
    HidReportDescriptor, HidReportError, USAGE_PAGE_BUTTON, USAGE_PAGE_CONSUMER,
    USAGE_PAGE_GENERIC_DESKTOP, USAGE_PAGE_KEYBOARD,
};
use crate::usb_hid::runtime_adapter::runtime_input_from_usb_report;
use crate::usb_hid::source::{UsbHidInputReport, UsbHidInterfaceSnapshot, UsbHidSourceError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHidInterfaceRuntimeDescriptorError {
    HidReport(HidReportError),
    KeyboardLedOutput(KeyboardLedOutputError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHidInterfaceRuntimeInputError {
    Source(UsbHidSourceError),
    WrongDevice,
    WrongInterface,
    Decode(UsbInputFrameError),
}

impl From<UsbHidSourceError> for UsbHidInterfaceRuntimeInputError {
    fn from(error: UsbHidSourceError) -> Self {
        Self::Source(error)
    }
}

impl From<UsbInputFrameError> for UsbHidInterfaceRuntimeInputError {
    fn from(error: UsbInputFrameError) -> Self {
        Self::Decode(error)
    }
}

impl From<HidReportError> for UsbHidInterfaceRuntimeDescriptorError {
    fn from(error: HidReportError) -> Self {
        Self::HidReport(error)
    }
}

impl From<KeyboardLedOutputError> for UsbHidInterfaceRuntimeDescriptorError {
    fn from(error: KeyboardLedOutputError) -> Self {
        Self::KeyboardLedOutput(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbHidInterfaceRuntimeSession<const FIELDS: usize, const EVENTS: usize> {
    source: UsbHidInterfaceSnapshot,
    descriptor: HidReportDescriptor<FIELDS>,
    led_output: Option<KeyboardLedOutputReport>,
}

impl<const FIELDS: usize, const EVENTS: usize> UsbHidInterfaceRuntimeSession<FIELDS, EVENTS> {
    pub fn from_source_snapshot(
        source: UsbHidInterfaceSnapshot,
        descriptor: HidReportDescriptor<FIELDS>,
        boot_keyboard_led_fallback: bool,
    ) -> Result<Self, UsbHidInterfaceRuntimeDescriptorError> {
        let has_keyboard_input = descriptor
            .fields()
            .any(|field| field.usage_page == USAGE_PAGE_KEYBOARD && !field.is_constant());
        let led_output = if has_keyboard_input {
            match KeyboardLedOutputReport::from_report_descriptor(source.report_descriptor()) {
                Ok(report) => Some(report),
                Err(KeyboardLedOutputError::MissingLedUsages) => {
                    boot_keyboard_led_fallback.then_some(KeyboardLedOutputReport::boot_keyboard())
                }
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };

        Ok(Self {
            source,
            descriptor,
            led_output,
        })
    }

    pub const fn interface_id(&self) -> InterfaceId {
        self.source.interface_id
    }

    pub const fn device_id(&self) -> DeviceId {
        self.source.device_id
    }

    pub const fn source_snapshot(&self) -> &UsbHidInterfaceSnapshot {
        &self.source
    }

    pub const fn descriptor(&self) -> &HidReportDescriptor<FIELDS> {
        &self.descriptor
    }

    pub const fn led_output(&self) -> Option<KeyboardLedOutputReport> {
        self.led_output
    }

    /// Management device kind bits: keyboard, mouse, consumer control.
    pub fn device_kind_flags(&self) -> u8 {
        let mut flags = 0;
        for field in self
            .descriptor
            .fields()
            .filter(|field| !field.is_constant())
        {
            match field.usage_page {
                USAGE_PAGE_KEYBOARD => flags |= 0x02,
                USAGE_PAGE_BUTTON | USAGE_PAGE_GENERIC_DESKTOP => flags |= 0x04,
                USAGE_PAGE_CONSUMER => flags |= 0x08,
                _ => {}
            }
        }
        flags
    }

    pub fn connected_message(&self) -> RuntimeInputMessage {
        RuntimeInputMessage::UsbHidInterfaceConnected {
            interface_id: self.interface_id(),
            device_id: self.device_id(),
            led_output: self.led_output,
        }
    }

    pub fn capture_input_report<'a>(
        &self,
        report: &'a [u8],
    ) -> Result<UsbHidInputReport<'a>, UsbHidSourceError> {
        UsbHidInputReport::new(self.device_id(), self.interface_id(), report)
    }

    pub fn input_message(
        &self,
        report: UsbHidInputReport<'_>,
    ) -> Result<RuntimeInputMessage, UsbHidInterfaceRuntimeInputError> {
        if report.device_id != self.device_id() {
            return Err(UsbHidInterfaceRuntimeInputError::WrongDevice);
        }
        if report.interface_id != self.interface_id() {
            return Err(UsbHidInterfaceRuntimeInputError::WrongInterface);
        }
        Ok(runtime_input_from_usb_report::<FIELDS, EVENTS>(
            self.device_id(),
            self.interface_id(),
            &self.descriptor,
            report.bytes(),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::BridgeEvent;
    use crate::input::InputFrame;
    use crate::runtime::RuntimeInput;
    use crate::usb_hid::report::{FIELD_FLAG_VARIABLE, ReportField, USAGE_PAGE_KEYBOARD};

    #[test]
    fn session_connected_message_uses_descriptor_led_layout() {
        let report_descriptor = keyboard_led_output_descriptor();
        let session = UsbHidInterfaceRuntimeSession::<4, 8>::from_source_snapshot(
            source_snapshot(DeviceId(7), InterfaceId(1), &report_descriptor),
            descriptor_with_keyboard_and_leds(),
            false,
        )
        .unwrap();

        assert_eq!(
            session.connected_message(),
            RuntimeInputMessage::UsbHidInterfaceConnected {
                interface_id: InterfaceId(1),
                device_id: DeviceId(7),
                led_output: Some(KeyboardLedOutputReport {
                    report_id: Some(2),
                    byte_len: 2,
                    num_lock_bit: Some(crate::usb_hid::output::BitPos::new(0, 0)),
                    caps_lock_bit: Some(crate::usb_hid::output::BitPos::new(0, 1)),
                    scroll_lock_bit: Some(crate::usb_hid::output::BitPos::new(0, 2)),
                }),
            }
        );
    }

    #[test]
    fn non_led_keyboard_does_not_use_boot_led_fallback() {
        let session = UsbHidInterfaceRuntimeSession::<2, 8>::from_source_snapshot(
            source_snapshot(DeviceId(3), InterfaceId(1), &[]),
            descriptor_with_keyboard_only(),
            false,
        )
        .unwrap();

        assert_eq!(session.led_output(), None);
    }

    #[test]
    fn boot_keyboard_can_use_boot_led_fallback() {
        let session = UsbHidInterfaceRuntimeSession::<2, 8>::from_source_snapshot(
            source_snapshot(DeviceId(3), InterfaceId(1), &[]),
            descriptor_with_keyboard_only(),
            true,
        )
        .unwrap();

        assert_eq!(
            session.led_output(),
            Some(KeyboardLedOutputReport::boot_keyboard())
        );
    }

    #[test]
    fn session_without_keyboard_fields_does_not_register_led_output() {
        let report_descriptor = mouse_led_output_descriptor();
        let session = UsbHidInterfaceRuntimeSession::<1, 8>::from_source_snapshot(
            source_snapshot(DeviceId(5), InterfaceId(3), &report_descriptor),
            descriptor_with_mouse_only(),
            false,
        )
        .unwrap();

        assert_eq!(session.led_output(), None);
    }

    #[test]
    fn device_kind_flags_are_derived_from_report_fields_not_led_support() {
        let keyboard = UsbHidInterfaceRuntimeSession::<2, 8>::from_source_snapshot(
            source_snapshot(DeviceId(3), InterfaceId(1), &[]),
            descriptor_with_keyboard_only(),
            false,
        )
        .unwrap();
        let mouse = UsbHidInterfaceRuntimeSession::<1, 8>::from_source_snapshot(
            source_snapshot(DeviceId(4), InterfaceId(2), &[]),
            descriptor_with_mouse_only(),
            false,
        )
        .unwrap();
        assert_eq!(keyboard.device_kind_flags(), 0x02);
        assert_eq!(mouse.device_kind_flags(), 0x04);
    }

    #[test]
    fn session_input_message_matches_runtime_bridge_event_adapter() {
        let session = UsbHidInterfaceRuntimeSession::<2, 8>::from_source_snapshot(
            source_snapshot(DeviceId(9), InterfaceId(2), &[]),
            descriptor_with_keyboard_only(),
            true,
        )
        .unwrap();

        let bytes = [0, 0, 0x04, 0, 0, 0, 0, 0];
        let report = session.capture_input_report(&bytes).unwrap();
        let input = session.input_message(report).unwrap();

        let RuntimeInput::BridgeEvent(BridgeEvent::InputFrame(InputFrame::Standard(frame))) =
            input.as_runtime_input()
        else {
            panic!("expected standard input frame");
        };

        assert_eq!(frame.device_id, DeviceId(9));
        assert_eq!(
            frame.keyboard.unwrap().keys_down(),
            &[crate::input::KeyUsage(0x04)]
        );
    }

    #[test]
    fn direct_adapter_rejects_a_report_from_another_source() {
        let session = UsbHidInterfaceRuntimeSession::<2, 8>::from_source_snapshot(
            source_snapshot(DeviceId(9), InterfaceId(2), &[]),
            descriptor_with_keyboard_only(),
            true,
        )
        .unwrap();

        assert_eq!(
            session.input_message(
                UsbHidInputReport::new(DeviceId(8), InterfaceId(2), &[0; 8]).unwrap()
            ),
            Err(UsbHidInterfaceRuntimeInputError::WrongDevice)
        );
        assert_eq!(
            session.input_message(
                UsbHidInputReport::new(DeviceId(9), InterfaceId(3), &[0; 8]).unwrap()
            ),
            Err(UsbHidInterfaceRuntimeInputError::WrongInterface)
        );
    }

    fn source_snapshot(
        device_id: DeviceId,
        interface_id: InterfaceId,
        descriptor: &[u8],
    ) -> UsbHidInterfaceSnapshot {
        UsbHidInterfaceSnapshot::new(device_id, interface_id, interface_id.0, 0, 0, 0, descriptor)
            .unwrap()
    }

    fn descriptor_with_keyboard_only() -> HidReportDescriptor<2> {
        let mut descriptor = HidReportDescriptor::new(false);
        descriptor
            .push(ReportField {
                report_id: 0,
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
                report_id: 0,
                usage_page: USAGE_PAGE_KEYBOARD,
                usage_min: 0x00,
                usage_max: 0xff,
                bit_offset: 16,
                bit_size: 8,
                count: 6,
                flags: 0,
                logical_min: 0,
                logical_max: 255,
            })
            .unwrap();
        descriptor
    }

    fn descriptor_with_keyboard_and_leds() -> HidReportDescriptor<4> {
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
                usage_min: 0x00,
                usage_max: 0xff,
                bit_offset: 16,
                bit_size: 8,
                count: 6,
                flags: 0,
                logical_min: 0,
                logical_max: 255,
            })
            .unwrap();
        descriptor
    }

    fn keyboard_led_output_descriptor() -> [u8; 25] {
        [
            0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x02, 0x05, 0x08, 0x19, 0x01, 0x29, 0x03,
            0x75, 0x01, 0x95, 0x03, 0x91, 0x02, 0x95, 0x05, 0x91, 0x01, 0xc0,
        ]
    }

    fn mouse_led_output_descriptor() -> [u8; 21] {
        [
            0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x04, 0x05, 0x08, 0x19, 0x01, 0x29, 0x03,
            0x75, 0x01, 0x95, 0x03, 0x91, 0x02, 0xc0,
        ]
    }

    fn descriptor_with_mouse_only() -> HidReportDescriptor<1> {
        let mut descriptor = HidReportDescriptor::new(false);
        descriptor
            .push(ReportField {
                report_id: 0,
                usage_page: crate::usb_hid::report::USAGE_PAGE_BUTTON,
                usage_min: 1,
                usage_max: 3,
                bit_offset: 0,
                bit_size: 1,
                count: 3,
                flags: FIELD_FLAG_VARIABLE,
                logical_min: 0,
                logical_max: 1,
            })
            .unwrap();
        descriptor
    }
}
