use crate::ids::{DeviceId, InterfaceId};
use crate::input::{
    ConsumerFrame, ConsumerUsage, InputError, KeyUsage, KeyboardFrame, Modifier, ModifierState,
    MouseButtons, MouseFrame, MouseMovement, StandardInputFrame,
};
use crate::usb_hid::report::{HidReportDescriptor, HidReportError};
use crate::usb_hid::report::{HidReportEvent, HidReportEvents};

pub fn decode_standard_input_frame<const FIELDS: usize, const EVENTS: usize>(
    device_id: DeviceId,
    interface_id: InterfaceId,
    descriptor: &HidReportDescriptor<FIELDS>,
    report: &[u8],
) -> Result<StandardInputFrame, UsbInputFrameError> {
    let mut events = HidReportEvents::<EVENTS>::new();
    let (report_id, _) = descriptor.report_id_for(report)?;
    let domains = descriptor.report_domains(report_id);
    crate::usb_hid::report::decode_report(descriptor, report, &mut events)?;
    let mut frame = events_to_standard_input_frame(device_id, interface_id, &events)
        .map_err(UsbInputFrameError::Input)?;

    if domains.keyboard && frame.keyboard.is_none() {
        frame.keyboard = Some(KeyboardFrame::new(ModifierState::empty()));
    }
    if domains.mouse && frame.mouse.is_none() {
        frame.mouse = Some(MouseFrame {
            buttons: MouseButtons::empty(),
            movement: MouseMovement::neutral(),
        });
    }
    if domains.consumer && frame.consumer.is_none() {
        frame.consumer = Some(ConsumerFrame { active: None });
    }

    Ok(frame)
}

pub fn events_to_standard_input_frame<const EVENTS: usize>(
    device_id: DeviceId,
    interface_id: InterfaceId,
    events: &HidReportEvents<EVENTS>,
) -> Result<StandardInputFrame, InputError> {
    let mut modifiers = ModifierState::empty();
    let mut keyboard = KeyboardFrame::new(ModifierState::empty());
    let mut has_keyboard = false;
    let mut mouse_buttons = MouseButtons::empty();
    let mut mouse_movement = MouseMovement::neutral();
    let mut has_mouse = false;
    let mut consumer = None;

    for event in events.iter() {
        match event {
            HidReportEvent::KeyboardUsageDown(usage) => {
                has_keyboard = true;
                if let Some(modifier) = modifier_from_keyboard_usage(usage) {
                    modifiers.set_modifier(modifier, true);
                } else if usage >= 4 {
                    keyboard.push_key(KeyUsage(usage))?;
                }
            }
            HidReportEvent::MouseButtonDown(button) => {
                has_mouse = true;
                mouse_buttons.set(button, true);
            }
            HidReportEvent::MouseX(x) => {
                has_mouse = true;
                mouse_movement.x = clamp_i32_to_i16(x);
            }
            HidReportEvent::MouseY(y) => {
                has_mouse = true;
                mouse_movement.y = clamp_i32_to_i16(y);
            }
            HidReportEvent::MouseWheel(wheel) => {
                has_mouse = true;
                mouse_movement.wheel = clamp_i32_to_i8(wheel);
            }
            HidReportEvent::MousePan(pan) => {
                has_mouse = true;
                mouse_movement.pan = clamp_i32_to_i8(pan);
            }
            HidReportEvent::ConsumerUsageDown(usage) => {
                if consumer.is_none() {
                    consumer = Some(ConsumerUsage(usage));
                }
            }
        }
    }

    keyboard.modifiers = modifiers;

    Ok(StandardInputFrame {
        device_id,
        interface_id,
        keyboard: has_keyboard.then_some(keyboard),
        mouse: has_mouse.then_some(MouseFrame {
            buttons: mouse_buttons,
            movement: mouse_movement,
        }),
        consumer: consumer.map(|active| ConsumerFrame {
            active: Some(active),
        }),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbInputFrameError {
    HidReport(HidReportError),
    Input(InputError),
}

impl From<HidReportError> for UsbInputFrameError {
    fn from(error: HidReportError) -> Self {
        Self::HidReport(error)
    }
}

const fn modifier_from_keyboard_usage(usage: u8) -> Option<Modifier> {
    match usage {
        0xe0 => Some(Modifier::LeftCtrl),
        0xe1 => Some(Modifier::LeftShift),
        0xe2 => Some(Modifier::LeftAlt),
        0xe3 => Some(Modifier::LeftGui),
        0xe4 => Some(Modifier::RightCtrl),
        0xe5 => Some(Modifier::RightShift),
        0xe6 => Some(Modifier::RightAlt),
        0xe7 => Some(Modifier::RightGui),
        _ => None,
    }
}

const fn clamp_i32_to_i16(value: i32) -> i16 {
    if value > i16::MAX as i32 {
        i16::MAX
    } else if value < i16::MIN as i32 {
        i16::MIN
    } else {
        value as i16
    }
}

const fn clamp_i32_to_i8(value: i32) -> i8 {
    if value > i8::MAX as i32 {
        i8::MAX
    } else if value < i8::MIN as i32 {
        i8::MIN
    } else {
        value as i8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::{Bridge, BridgeAction, BridgeEvent, NotifyReason};
    use crate::input::{
        ConsumerFrame, ConsumerUsage, KeyUsage, ModifierState, MouseButton, MouseButtons,
        MouseFrame, MouseMovement,
    };
    use crate::output_target::OutputTarget;
    use crate::reports::{Keyboard6KroReport, ReportKind, StandardHidReport};
    use crate::usb_hid::report::{
        FIELD_FLAG_VARIABLE, ReportField, USAGE_PAGE_BUTTON, USAGE_PAGE_GENERIC_DESKTOP,
        USAGE_PAGE_KEYBOARD, USAGE_X, USAGE_Y,
    };
    use crate::usb_hid::report::{HidReportEvent, HidReportEvents};
    use crate::usb_hid::test_fixtures;

    const DEVICE: DeviceId = DeviceId(7);
    const INTERFACE: InterfaceId = InterfaceId(3);
    const HOST: crate::ids::HostId = crate::ids::HostId(1);

    #[test]
    fn converts_hid_report_events_into_standard_input_frame() {
        let mut events = HidReportEvents::<8>::new();
        events.push_for_test(HidReportEvent::KeyboardUsageDown(0xe1));
        events.push_for_test(HidReportEvent::KeyboardUsageDown(0x04));
        events.push_for_test(HidReportEvent::KeyboardUsageDown(0x05));
        events.push_for_test(HidReportEvent::MouseX(12));
        events.push_for_test(HidReportEvent::MouseY(-3));
        events.push_for_test(HidReportEvent::MouseButtonDown(MouseButton::Left));
        events.push_for_test(HidReportEvent::ConsumerUsageDown(0x00e9));

        let frame = events_to_standard_input_frame(DEVICE, INTERFACE, &events).unwrap();

        assert_eq!(frame.device_id, DEVICE);
        assert_eq!(
            frame.keyboard.as_ref().map(|keyboard| keyboard.modifiers),
            Some(ModifierState::LEFT_SHIFT)
        );
        assert_eq!(
            frame.keyboard.as_ref().map(|keyboard| keyboard.keys_down()),
            Some(&[KeyUsage(0x04), KeyUsage(0x05)][..])
        );
        assert_eq!(
            frame.mouse,
            Some(MouseFrame {
                buttons: mouse_buttons(&[MouseButton::Left]),
                movement: MouseMovement {
                    x: 12,
                    y: -3,
                    wheel: 0,
                    pan: 0,
                },
            })
        );
        assert_eq!(
            frame.consumer,
            Some(ConsumerFrame {
                active: Some(ConsumerUsage(0x00e9)),
            })
        );
    }

    #[test]
    fn events_without_a_domain_do_not_create_empty_subframes() {
        let events = HidReportEvents::<4>::new();

        let frame = events_to_standard_input_frame(DEVICE, INTERFACE, &events).unwrap();

        assert_eq!(frame.device_id, DEVICE);
        assert!(frame.keyboard.is_none());
        assert!(frame.mouse.is_none());
        assert!(frame.consumer.is_none());
    }

    #[test]
    fn decodes_usb_report_and_drives_bridge_keyboard_notification() {
        let descriptor = keyboard_descriptor();

        let frame = decode_standard_input_frame::<3, 8>(
            DEVICE,
            INTERFACE,
            &descriptor,
            &[0b0000_0010, 0, 0x04, 0x05, 0, 0, 0, 0],
        )
        .unwrap();

        let mut bridge = ready_keyboard_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(frame)),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::Notify {
                target: OutputTarget::Ble(HOST),
                report: StandardHidReport::Keyboard(
                    Keyboard6KroReport::from_visible_state(
                        &bridge
                            .state()
                            .input
                            .keyboard
                            .visible_against(&bridge.state().suppression.keyboard)
                    )
                    .report
                ),
                reason: NotifyReason::Input,
            }]
        );
    }

    #[test]
    fn decodes_usb_keyboard_release_and_drives_bridge_release_notification() {
        let descriptor = keyboard_descriptor();
        let mut bridge = ready_keyboard_bridge();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();

        let press_frame = decode_standard_input_frame::<3, 8>(
            DEVICE,
            INTERFACE,
            &descriptor,
            &[0, 0, 0x04, 0, 0, 0, 0, 0],
        )
        .unwrap();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(press_frame)),
                &mut actions,
            )
            .unwrap();
        actions.clear();

        let release_frame = decode_standard_input_frame::<3, 8>(
            DEVICE,
            INTERFACE,
            &descriptor,
            &[0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();
        bridge
            .handle_event(
                BridgeEvent::InputFrame(crate::input::InputFrame::Standard(release_frame)),
                &mut actions,
            )
            .unwrap();

        assert_eq!(
            actions.as_slice(),
            &[BridgeAction::Notify {
                target: OutputTarget::Ble(HOST),
                report: StandardHidReport::Keyboard(Keyboard6KroReport::release()),
                reason: NotifyReason::InputRelease,
            }]
        );
    }

    #[test]
    fn decodes_empty_keyboard_report_as_keyboard_release_frame() {
        let descriptor = keyboard_descriptor();

        let frame = decode_standard_input_frame::<3, 8>(
            DEVICE,
            INTERFACE,
            &descriptor,
            &[0, 0, 0, 0, 0, 0, 0, 0],
        )
        .unwrap();

        let keyboard = frame.keyboard.expect("keyboard frame");
        assert_eq!(keyboard.modifiers, ModifierState::empty());
        assert!(keyboard.keys_down().is_empty());
        assert!(frame.mouse.is_none());
        assert!(frame.consumer.is_none());
    }

    #[test]
    fn decodes_zero_mouse_button_report_as_mouse_release_frame() {
        let mut descriptor = HidReportDescriptor::<2>::new(false);
        descriptor
            .push(ReportField {
                report_id: 0,
                usage_page: USAGE_PAGE_BUTTON,
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

        let frame =
            decode_standard_input_frame::<2, 8>(DEVICE, INTERFACE, &descriptor, &[0]).unwrap();

        assert_eq!(
            frame.mouse,
            Some(MouseFrame {
                buttons: MouseButtons::empty(),
                movement: MouseMovement::neutral(),
            })
        );
        assert!(frame.keyboard.is_none());
        assert!(frame.consumer.is_none());
    }

    #[test]
    fn decodes_zero_consumer_array_report_as_consumer_release_frame() {
        let mut descriptor = HidReportDescriptor::<1>::new(false);
        descriptor
            .push(ReportField {
                report_id: 0,
                usage_page: crate::usb_hid::report::USAGE_PAGE_CONSUMER,
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

        let frame =
            decode_standard_input_frame::<1, 8>(DEVICE, INTERFACE, &descriptor, &[0, 0]).unwrap();

        assert_eq!(frame.consumer, Some(ConsumerFrame { active: None }));
        assert!(frame.keyboard.is_none());
        assert!(frame.mouse.is_none());
    }

    #[test]
    fn decodes_usb_report_mouse_buttons_and_axes_into_frame() {
        let mut descriptor = HidReportDescriptor::<2>::new(false);
        descriptor
            .push(ReportField {
                report_id: 0,
                usage_page: USAGE_PAGE_BUTTON,
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
            .push(ReportField {
                report_id: 0,
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

        let frame = decode_standard_input_frame::<2, 8>(
            DEVICE,
            INTERFACE,
            &descriptor,
            &[0b0000_0101, 0x7f, 0x80],
        )
        .unwrap();

        assert_eq!(
            frame.mouse,
            Some(MouseFrame {
                buttons: mouse_buttons(&[MouseButton::Left, MouseButton::Middle]),
                movement: MouseMovement {
                    x: 127,
                    y: -128,
                    wheel: 0,
                    pan: 0,
                },
            })
        );
    }

    #[test]
    fn golden_composite_mouse_report_maps_pan_without_consumer_frame() {
        let descriptor = test_fixtures::composite_report_id_descriptor();

        let frame = decode_standard_input_frame::<8, 8>(
            DEVICE,
            INTERFACE,
            &descriptor,
            &test_fixtures::mouse_report(0b0000_0001, 10, -4, 2, -3),
        )
        .unwrap();

        assert_eq!(
            frame.mouse,
            Some(MouseFrame {
                buttons: mouse_buttons(&[MouseButton::Left]),
                movement: MouseMovement {
                    x: 10,
                    y: -4,
                    wheel: 2,
                    pan: -3,
                },
            })
        );
        assert!(frame.consumer.is_none());
        assert!(frame.keyboard.is_none());
    }

    #[test]
    fn golden_composite_consumer_report_stays_out_of_mouse_domain() {
        let descriptor = test_fixtures::consumer_array_descriptor();

        let frame = decode_standard_input_frame::<1, 8>(
            DEVICE,
            INTERFACE,
            &descriptor,
            &test_fixtures::consumer_report(0x00e2),
        )
        .unwrap();

        assert_eq!(
            frame.consumer,
            Some(ConsumerFrame {
                active: Some(ConsumerUsage(0x00e2)),
            })
        );
        assert!(frame.mouse.is_none());
    }

    fn ready_keyboard_bridge() -> Bridge<1> {
        let mut bridge = Bridge::<1>::new();
        let mut actions = heapless::Vec::<BridgeAction, 4>::new();
        bridge
            .handle_event(BridgeEvent::HostConnected { host_id: HOST }, &mut actions)
            .unwrap();
        bridge
            .handle_event(
                BridgeEvent::HostSecurityChanged {
                    host_id: HOST,
                    encrypted: true,
                    bonded: true,
                    bond: None,
                },
                &mut actions,
            )
            .unwrap();
        bridge
            .handle_event(
                BridgeEvent::CccdChanged {
                    host_id: HOST,
                    report: ReportKind::Keyboard,
                    enabled: true,
                },
                &mut actions,
            )
            .unwrap();
        bridge
            .handle_event(BridgeEvent::SwitchTarget { target: HOST }, &mut actions)
            .unwrap();
        bridge
    }

    fn keyboard_descriptor() -> HidReportDescriptor<3> {
        let mut descriptor = HidReportDescriptor::<3>::new(false);
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

    fn mouse_buttons(buttons: &[MouseButton]) -> MouseButtons {
        let mut state = MouseButtons::empty();
        for button in buttons {
            state.set(*button, true);
        }
        state
    }
}
