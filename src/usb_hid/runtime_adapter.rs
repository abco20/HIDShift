use crate::bridge::BridgeEvent;
use crate::ids::DeviceId;
use crate::runtime::message::RuntimeInputMessage;
use crate::usb_hid::frame::{UsbInputFrameError, decode_standard_input_frame};
use crate::usb_hid::report::HidReportDescriptor;

pub fn runtime_input_from_usb_report<const FIELDS: usize, const EVENTS: usize>(
    device_id: DeviceId,
    descriptor: &HidReportDescriptor<FIELDS>,
    report: &[u8],
) -> Result<RuntimeInputMessage, UsbInputFrameError> {
    let frame = decode_standard_input_frame::<FIELDS, EVENTS>(device_id, descriptor, report)?;
    Ok(RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(
        crate::input::InputFrame::Standard(frame),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::HostId;
    use crate::reports::{BleHidReport, ReportKind};
    use crate::runtime::BridgeRuntime;
    use crate::runtime::message::RuntimeInputMessage;
    use crate::usb_hid::report::{
        FIELD_FLAG_VARIABLE, HidReportDescriptor, ReportField, USAGE_PAGE_KEYBOARD,
    };
    use crate::usb_hid::test_fixtures;

    const DEVICE: DeviceId = DeviceId(7);

    #[test]
    fn usb_report_becomes_runtime_input_message() {
        let descriptor = keyboard_descriptor();

        let input = runtime_input_from_usb_report::<3, 8>(
            DEVICE,
            &descriptor,
            &[0, 0, 0x04, 0, 0, 0, 0, 0],
        )
        .unwrap();

        let RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(
            crate::input::InputFrame::Standard(frame),
        )) = input
        else {
            panic!("expected runtime bridge event");
        };

        assert_eq!(frame.device_id, DEVICE);
        assert_eq!(
            frame.keyboard.expect("keyboard").keys_down(),
            &[crate::input::KeyUsage(0x04)]
        );
    }

    #[test]
    fn usb_runtime_input_drives_bridge_notify_when_host_is_ready() {
        let descriptor = keyboard_descriptor();
        let input = runtime_input_from_usb_report::<3, 8>(
            DEVICE,
            &descriptor,
            &[0, 0, 0x04, 0, 0, 0, 0, 0],
        )
        .unwrap();
        let mut runtime = BridgeRuntime::<1, 0>::new(0);
        let mut commands = heapless::Vec::new();

        for event in [
            crate::bridge::BridgeEvent::HostConnected { host_id: HostId(1) },
            crate::bridge::BridgeEvent::HostSecurityChanged {
                host_id: HostId(1),
                encrypted: true,
                bonded: true,
                bond: None,
            },
            crate::bridge::BridgeEvent::CccdChanged {
                host_id: HostId(1),
                report: ReportKind::Keyboard,
                enabled: true,
            },
            crate::bridge::BridgeEvent::SwitchTarget { target: HostId(1) },
        ] {
            runtime
                .handle_input::<8, 8, 2>(
                    RuntimeInputMessage::BridgeEvent(event).as_runtime_input(),
                    &mut commands,
                )
                .unwrap();
        }

        runtime
            .handle_input::<8, 8, 2>(input.as_runtime_input(), &mut commands)
            .unwrap();

        assert!(matches!(
            commands.as_slice(),
            [crate::runtime::RuntimeCommand::BleCommand(
                crate::runtime::BleTaskCommand::Notify {
                    report: BleHidReport::Keyboard(_),
                    ..
                }
            )]
        ));
    }

    #[test]
    fn composite_mouse_report_becomes_runtime_input_with_pan() {
        let descriptor = test_fixtures::composite_report_id_descriptor();

        let input = runtime_input_from_usb_report::<8, 8>(
            DEVICE,
            &descriptor,
            &test_fixtures::mouse_report(0b0000_0001, 4, -1, 0, -2),
        )
        .unwrap();

        let RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(
            crate::input::InputFrame::Standard(frame),
        )) = input
        else {
            panic!("expected runtime bridge event");
        };

        let mouse = frame.mouse.expect("mouse");
        assert_eq!(mouse.movement.pan, -2);
        assert!(frame.consumer.is_none());
    }

    fn keyboard_descriptor() -> HidReportDescriptor<3> {
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
                usage_min: 0,
                usage_max: 0,
                bit_offset: 8,
                bit_size: 8,
                count: 1,
                flags: 0,
                logical_min: 0,
                logical_max: 0,
            })
            .unwrap();
        descriptor
            .push(ReportField {
                report_id: 0,
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
    }
}
