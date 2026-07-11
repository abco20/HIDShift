use crate::ids::{DeviceId, InterfaceId};
use crate::runtime::message::RuntimeInputMessage;
use crate::usb_hid::frame::UsbInputFrameError;
use crate::usb_hid::output::{KeyboardLedOutputError, KeyboardLedOutputReport};
use crate::usb_hid::report::{
    HidReportDescriptor, HidReportError, USAGE_PAGE_BUTTON, USAGE_PAGE_CONSUMER,
    USAGE_PAGE_GENERIC_DESKTOP, USAGE_PAGE_KEYBOARD,
};
use crate::usb_hid::runtime_adapter::runtime_input_from_usb_report;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbHidInterfaceRuntimeDescriptorError {
    HidReport(HidReportError),
    KeyboardLedOutput(KeyboardLedOutputError),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbHidInterfaceRuntimeSession<const FIELDS: usize, const EVENTS: usize> {
    interface_id: InterfaceId,
    device_id: DeviceId,
    descriptor: HidReportDescriptor<FIELDS>,
    led_output: Option<KeyboardLedOutputReport>,
}

impl<const FIELDS: usize, const EVENTS: usize> UsbHidInterfaceRuntimeSession<FIELDS, EVENTS> {
    pub fn from_core_descriptor(
        interface_id: InterfaceId,
        device_id: DeviceId,
        descriptor: HidReportDescriptor<FIELDS>,
        report_descriptor: &[u8],
        boot_keyboard_led_fallback: bool,
    ) -> Result<Self, UsbHidInterfaceRuntimeDescriptorError> {
        let has_keyboard_input = descriptor
            .fields()
            .any(|field| field.usage_page == USAGE_PAGE_KEYBOARD && !field.is_constant());
        let led_output = if has_keyboard_input {
            match KeyboardLedOutputReport::from_report_descriptor(report_descriptor) {
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
            interface_id,
            device_id,
            descriptor,
            led_output,
        })
    }

    #[cfg(feature = "usb-host")]
    pub fn from_embassy_descriptor<const N: usize>(
        interface_id: InterfaceId,
        device_id: DeviceId,
        descriptor: &embassy_usb_host::class::hid::ReportDescriptor<N>,
        report_descriptor: &[u8],
        boot_keyboard_led_fallback: bool,
    ) -> Result<Self, UsbHidInterfaceRuntimeDescriptorError> {
        let descriptor = to_core_descriptor::<N, FIELDS>(descriptor)?;
        Self::from_core_descriptor(
            interface_id,
            device_id,
            descriptor,
            report_descriptor,
            boot_keyboard_led_fallback,
        )
    }

    pub const fn interface_id(&self) -> InterfaceId {
        self.interface_id
    }

    pub const fn device_id(&self) -> DeviceId {
        self.device_id
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
            interface_id: self.interface_id,
            device_id: self.device_id,
            led_output: self.led_output,
        }
    }

    pub fn input_message(&self, report: &[u8]) -> Result<RuntimeInputMessage, UsbInputFrameError> {
        runtime_input_from_usb_report::<FIELDS, EVENTS>(
            self.device_id,
            self.interface_id,
            &self.descriptor,
            report,
        )
    }
}

#[cfg(feature = "usb-host")]
pub fn to_core_descriptor<const SRC: usize, const DST: usize>(
    descriptor: &embassy_usb_host::class::hid::ReportDescriptor<SRC>,
) -> Result<HidReportDescriptor<DST>, HidReportError> {
    let mut core_descriptor = HidReportDescriptor::new(descriptor.has_report_ids);
    for field in descriptor.fields() {
        core_descriptor.push(crate::usb_hid::report::ReportField {
            report_id: field.report_id,
            usage_page: field.usage_page,
            usage_min: field.usage_min,
            usage_max: field.usage_max,
            bit_offset: field.bit_offset,
            bit_size: field.bit_size,
            count: field.count,
            flags: field.flags,
            logical_min: field.logical_min,
            logical_max: field.logical_max,
        })?;
    }
    Ok(core_descriptor)
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
        let session = UsbHidInterfaceRuntimeSession::<4, 8>::from_core_descriptor(
            InterfaceId(1),
            DeviceId(7),
            descriptor_with_keyboard_and_leds(),
            &keyboard_led_output_descriptor(),
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
        let session = UsbHidInterfaceRuntimeSession::<2, 8>::from_core_descriptor(
            InterfaceId(1),
            DeviceId(3),
            descriptor_with_keyboard_only(),
            &[],
            false,
        )
        .unwrap();

        assert_eq!(session.led_output(), None);
    }

    #[test]
    fn boot_keyboard_can_use_boot_led_fallback() {
        let session = UsbHidInterfaceRuntimeSession::<2, 8>::from_core_descriptor(
            InterfaceId(1),
            DeviceId(3),
            descriptor_with_keyboard_only(),
            &[],
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
        let session = UsbHidInterfaceRuntimeSession::<1, 8>::from_core_descriptor(
            InterfaceId(3),
            DeviceId(5),
            descriptor_with_mouse_only(),
            &mouse_led_output_descriptor(),
            false,
        )
        .unwrap();

        assert_eq!(session.led_output(), None);
    }

    #[test]
    fn device_kind_flags_are_derived_from_report_fields_not_led_support() {
        let keyboard = UsbHidInterfaceRuntimeSession::<2, 8>::from_core_descriptor(
            InterfaceId(1),
            DeviceId(3),
            descriptor_with_keyboard_only(),
            &[],
            false,
        )
        .unwrap();
        let mouse = UsbHidInterfaceRuntimeSession::<1, 8>::from_core_descriptor(
            InterfaceId(2),
            DeviceId(4),
            descriptor_with_mouse_only(),
            &[],
            false,
        )
        .unwrap();
        assert_eq!(keyboard.device_kind_flags(), 0x02);
        assert_eq!(mouse.device_kind_flags(), 0x04);
    }

    #[test]
    fn session_input_message_matches_runtime_bridge_event_adapter() {
        let session = UsbHidInterfaceRuntimeSession::<2, 8>::from_core_descriptor(
            InterfaceId(2),
            DeviceId(9),
            descriptor_with_keyboard_only(),
            &[],
            true,
        )
        .unwrap();

        let input = session.input_message(&[0, 0, 0x04, 0, 0, 0, 0, 0]).unwrap();

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
