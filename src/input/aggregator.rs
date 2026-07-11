use crate::ids::{DeviceId, InterfaceId};

use super::{
    ConsumerState, InputError, MouseButtons, PhysicalInputState, PhysicalKeyboardState,
    StandardInputFrame,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeviceInputState {
    device_id: DeviceId,
    interface_id: InterfaceId,
    keyboard: PhysicalKeyboardState,
    mouse_buttons: MouseButtons,
    consumer: ConsumerState,
    consumer_generation: u64,
}

impl DeviceInputState {
    const fn new(device_id: DeviceId, interface_id: InterfaceId) -> Self {
        Self {
            device_id,
            interface_id,
            keyboard: PhysicalKeyboardState::new(),
            mouse_buttons: MouseButtons::empty(),
            consumer: ConsumerState::new(),
            consumer_generation: 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputAggregator<const DEVICES: usize> {
    devices: [Option<DeviceInputState>; DEVICES],
    aggregate: PhysicalInputState,
    next_consumer_generation: u64,
}

impl<const DEVICES: usize> InputAggregator<DEVICES> {
    pub const fn new() -> Self {
        Self {
            devices: [const { None }; DEVICES],
            aggregate: PhysicalInputState::new(),
            next_consumer_generation: 1,
        }
    }

    pub const fn aggregate(&self) -> &PhysicalInputState {
        &self.aggregate
    }

    pub fn apply_frame(&mut self, frame: &StandardInputFrame) -> Result<(), InputError> {
        // Relative movement is deliberately not persistent aggregator state.
        // An otherwise empty movement frame must not consume a device slot.
        let existing_index = interface_index(&self.devices, frame.interface_id);
        let existing_buttons = existing_index
            .and_then(|index| self.devices[index].as_ref())
            .map(|entry| entry.mouse_buttons)
            .unwrap_or_else(MouseButtons::empty);
        let buttons_changed = frame
            .mouse
            .is_some_and(|mouse| mouse.buttons != existing_buttons);
        if frame.keyboard.is_none() && frame.consumer.is_none() && !buttons_changed {
            return Ok(());
        }

        let mut devices = self.devices.clone();
        let index = match existing_index {
            Some(index) => index,
            None => self
                .devices
                .iter()
                .position(Option::is_none)
                .ok_or(InputError::DeviceCapacity)?,
        };

        let entry = devices[index]
            .get_or_insert(DeviceInputState::new(frame.device_id, frame.interface_id));

        if let Some(keyboard) = &frame.keyboard {
            entry.keyboard.replace_with_frame(keyboard)?;
        }
        if let Some(mouse) = frame.mouse {
            entry.mouse_buttons = mouse.buttons;
        }
        if let Some(consumer) = frame.consumer {
            entry.consumer.active = consumer.active;
            entry.consumer_generation = self.next_consumer_generation;
        }

        let mut aggregate = self.aggregate.clone();
        if frame.keyboard.is_some() {
            rebuild_keyboard(&devices, &mut aggregate)?;
        }
        if buttons_changed {
            rebuild_mouse_buttons(&devices, &mut aggregate);
        }
        if frame.consumer.is_some() {
            rebuild_consumer(&devices, &mut aggregate);
        }
        self.devices = devices;
        self.aggregate = aggregate;
        if frame.consumer.is_some() {
            self.next_consumer_generation = self.next_consumer_generation.wrapping_add(1);
        }
        Ok(())
    }

    pub fn remove_device(&mut self, device_id: DeviceId) -> bool {
        if !self
            .devices
            .iter()
            .flatten()
            .any(|entry| entry.device_id == device_id)
        {
            return false;
        }
        let mut devices = self.devices.clone();
        for entry in &mut devices {
            if entry
                .as_ref()
                .is_some_and(|entry| entry.device_id == device_id)
            {
                *entry = None;
            }
        }
        let Ok(aggregate) = rebuild_aggregate(&devices) else {
            return false;
        };
        self.devices = devices;
        self.aggregate = aggregate;
        true
    }

    pub fn remove_interface(&mut self, interface_id: InterfaceId) -> bool {
        let Some(index) = interface_index(&self.devices, interface_id) else {
            return false;
        };
        let mut devices = self.devices.clone();
        devices[index] = None;
        let Ok(aggregate) = rebuild_aggregate(&devices) else {
            return false;
        };
        self.devices = devices;
        self.aggregate = aggregate;
        true
    }
}

fn interface_index(
    devices: &[Option<DeviceInputState>],
    interface_id: InterfaceId,
) -> Option<usize> {
    devices
        .iter()
        .position(|entry| matches!(entry, Some(entry) if entry.interface_id == interface_id))
}

fn rebuild_aggregate(
    devices: &[Option<DeviceInputState>],
) -> Result<PhysicalInputState, InputError> {
    let mut aggregate = PhysicalInputState::new();
    let mut newest_consumer_generation = None;
    for entry in devices.iter().filter_map(|entry| entry.as_ref()) {
        aggregate.keyboard.modifiers |= entry.keyboard.modifiers;
        for key in entry.keyboard.keys().iter().copied() {
            aggregate.keyboard.press_key(key)?;
        }
        aggregate.mouse.buttons = MouseButtons::from_bits_truncate(
            aggregate.mouse.buttons.bits() | entry.mouse_buttons.bits(),
        );
        if entry.consumer.active.is_some()
            && newest_consumer_generation.is_none_or(|generation| {
                generation_is_newer_or_equal(entry.consumer_generation, generation)
            })
        {
            aggregate.consumer = entry.consumer;
            newest_consumer_generation = Some(entry.consumer_generation);
        }
    }
    Ok(aggregate)
}

fn rebuild_keyboard(
    devices: &[Option<DeviceInputState>],
    aggregate: &mut PhysicalInputState,
) -> Result<(), InputError> {
    aggregate.keyboard = PhysicalKeyboardState::new();
    for entry in devices.iter().filter_map(|entry| entry.as_ref()) {
        aggregate.keyboard.modifiers |= entry.keyboard.modifiers;
        for key in entry.keyboard.keys().iter().copied() {
            aggregate.keyboard.press_key(key)?;
        }
    }
    Ok(())
}

fn rebuild_mouse_buttons(devices: &[Option<DeviceInputState>], aggregate: &mut PhysicalInputState) {
    aggregate.mouse.buttons = MouseButtons::empty();
    for entry in devices.iter().filter_map(|entry| entry.as_ref()) {
        aggregate.mouse.buttons = MouseButtons::from_bits_truncate(
            aggregate.mouse.buttons.bits() | entry.mouse_buttons.bits(),
        );
    }
}

fn rebuild_consumer(devices: &[Option<DeviceInputState>], aggregate: &mut PhysicalInputState) {
    aggregate.consumer = ConsumerState::new();
    let mut newest_generation = None;
    for entry in devices.iter().filter_map(|entry| entry.as_ref()) {
        if entry.consumer.active.is_some()
            && newest_generation.is_none_or(|generation| {
                generation_is_newer_or_equal(entry.consumer_generation, generation)
            })
        {
            aggregate.consumer = entry.consumer;
            newest_generation = Some(entry.consumer_generation);
        }
    }
}

const fn generation_is_newer_or_equal(left: u64, right: u64) -> bool {
    left.wrapping_sub(right) < (1u64 << 63)
}

impl<const DEVICES: usize> Default for InputAggregator<DEVICES> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        ConsumerFrame, ConsumerUsage, KeyUsage, KeyboardFrame, ModifierState, MouseFrame,
        MouseMovement,
    };

    #[test]
    fn keyboard_union_survives_multiple_devices() {
        let mut aggregator = InputAggregator::<4>::new();
        let mut keyboard_a = KeyboardFrame::new(ModifierState::LEFT_SHIFT);
        keyboard_a.push_key(KeyUsage(0x04)).unwrap();
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(1),
                interface_id: InterfaceId(1),
                keyboard: Some(keyboard_a),
                mouse: None,
                consumer: None,
            })
            .unwrap();

        let mut keyboard_b = KeyboardFrame::new(ModifierState::RIGHT_ALT);
        keyboard_b.push_key(KeyUsage(0x05)).unwrap();
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(2),
                interface_id: InterfaceId(2),
                keyboard: Some(keyboard_b),
                mouse: None,
                consumer: None,
            })
            .unwrap();

        assert_eq!(
            aggregator.aggregate().keyboard.modifiers,
            ModifierState::LEFT_SHIFT | ModifierState::RIGHT_ALT
        );
        assert_eq!(
            aggregator.aggregate().keyboard.keys(),
            &[KeyUsage(0x04), KeyUsage(0x05)]
        );
    }

    #[test]
    fn removing_device_keeps_other_device_state() {
        let mut aggregator = InputAggregator::<4>::new();
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(1),
                interface_id: InterfaceId(1),
                keyboard: Some({
                    let mut frame = KeyboardFrame::new(ModifierState::empty());
                    frame.push_key(KeyUsage(0x04)).unwrap();
                    frame
                }),
                mouse: Some(MouseFrame {
                    buttons: MouseButtons::LEFT,
                    movement: MouseMovement::neutral(),
                }),
                consumer: None,
            })
            .unwrap();
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(2),
                interface_id: InterfaceId(2),
                keyboard: Some({
                    let mut frame = KeyboardFrame::new(ModifierState::empty());
                    frame.push_key(KeyUsage(0x05)).unwrap();
                    frame
                }),
                mouse: None,
                consumer: Some(ConsumerFrame {
                    active: Some(ConsumerUsage(0x00e9)),
                }),
            })
            .unwrap();

        assert!(aggregator.remove_device(DeviceId(1)));

        assert_eq!(aggregator.aggregate().keyboard.keys(), &[KeyUsage(0x05)]);
        assert_eq!(aggregator.aggregate().mouse.buttons, MouseButtons::empty());
        assert_eq!(
            aggregator.aggregate().consumer.active,
            Some(ConsumerUsage(0x00e9))
        );
    }

    #[test]
    fn newest_active_consumer_wins_and_falls_back_when_removed() {
        let mut aggregator = InputAggregator::<4>::new();
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(1),
                interface_id: InterfaceId(1),
                keyboard: None,
                mouse: None,
                consumer: Some(ConsumerFrame {
                    active: Some(ConsumerUsage(0x00e9)),
                }),
            })
            .unwrap();
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(2),
                interface_id: InterfaceId(2),
                keyboard: None,
                mouse: None,
                consumer: Some(ConsumerFrame {
                    active: Some(ConsumerUsage(0x00cd)),
                }),
            })
            .unwrap();

        assert_eq!(
            aggregator.aggregate().consumer.active,
            Some(ConsumerUsage(0x00cd))
        );

        aggregator.remove_device(DeviceId(2));
        assert_eq!(
            aggregator.aggregate().consumer.active,
            Some(ConsumerUsage(0x00e9))
        );
    }

    #[test]
    fn keyboard_and_mouse_frames_do_not_change_consumer_priority() {
        let mut aggregator = InputAggregator::<4>::new();
        for (device_id, usage) in [(1, 0x00e9), (2, 0x00cd)] {
            aggregator
                .apply_frame(&StandardInputFrame {
                    device_id: DeviceId(device_id),
                    interface_id: InterfaceId(device_id),
                    keyboard: None,
                    mouse: None,
                    consumer: Some(ConsumerFrame {
                        active: Some(ConsumerUsage(usage)),
                    }),
                })
                .unwrap();
        }

        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(1),
                interface_id: InterfaceId(1),
                keyboard: Some(KeyboardFrame::new(ModifierState::empty())),
                mouse: Some(MouseFrame {
                    buttons: MouseButtons::LEFT,
                    movement: MouseMovement {
                        x: 1,
                        y: 0,
                        wheel: 0,
                        pan: 0,
                    },
                }),
                consumer: None,
            })
            .unwrap();

        assert_eq!(
            aggregator.aggregate().consumer.active,
            Some(ConsumerUsage(0x00cd))
        );
    }

    #[test]
    fn movement_only_frames_do_not_consume_device_capacity() {
        let mut aggregator = InputAggregator::<1>::new();
        for device_id in 1..=8 {
            aggregator
                .apply_frame(&StandardInputFrame {
                    device_id: DeviceId(device_id),
                    interface_id: InterfaceId(device_id),
                    keyboard: None,
                    mouse: Some(MouseFrame {
                        buttons: MouseButtons::empty(),
                        movement: MouseMovement {
                            x: 1,
                            y: -1,
                            wheel: 0,
                            pan: 0,
                        },
                    }),
                    consumer: None,
                })
                .unwrap();
        }
        assert!(aggregator.devices.iter().all(Option::is_none));
    }

    #[test]
    fn consumer_generation_comparison_survives_wrap() {
        let mut aggregator = InputAggregator::<2>::new();
        aggregator.next_consumer_generation = u64::MAX;
        for (device_id, usage) in [(1, 0x00e9), (2, 0x00cd)] {
            aggregator
                .apply_frame(&StandardInputFrame {
                    device_id: DeviceId(device_id),
                    interface_id: InterfaceId(device_id),
                    keyboard: None,
                    mouse: None,
                    consumer: Some(ConsumerFrame {
                        active: Some(ConsumerUsage(usage)),
                    }),
                })
                .unwrap();
        }
        assert_eq!(
            aggregator.aggregate().consumer.active,
            Some(ConsumerUsage(0x00cd))
        );
    }

    #[test]
    fn capacity_error_does_not_partially_update_device_or_aggregate_state() {
        let mut aggregator = InputAggregator::<2>::new();
        let mut first = KeyboardFrame::new(ModifierState::empty());
        for usage in 4..=23 {
            first.push_key(KeyUsage(usage)).unwrap();
        }
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(1),
                interface_id: InterfaceId(1),
                keyboard: Some(first),
                mouse: None,
                consumer: None,
            })
            .unwrap();
        let before = aggregator.clone();
        let mut second = KeyboardFrame::new(ModifierState::empty());
        for usage in 24..=43 {
            second.push_key(KeyUsage(usage)).unwrap();
        }

        assert_eq!(
            aggregator.apply_frame(&StandardInputFrame {
                device_id: DeviceId(2),
                interface_id: InterfaceId(2),
                keyboard: Some(second),
                mouse: None,
                consumer: None,
            }),
            Err(InputError::KeyCapacity)
        );
        assert_eq!(aggregator, before);
    }

    #[test]
    fn same_device_keyboard_interfaces_are_aggregated_independently() {
        let mut aggregator = InputAggregator::<2>::new();
        for (interface, usage) in [(1, 0x04), (2, 0x05)] {
            let mut keyboard = KeyboardFrame::new(ModifierState::empty());
            keyboard.push_key(KeyUsage(usage)).unwrap();
            aggregator
                .apply_frame(&StandardInputFrame {
                    device_id: DeviceId(7),
                    interface_id: InterfaceId(interface),
                    keyboard: Some(keyboard),
                    mouse: None,
                    consumer: None,
                })
                .unwrap();
        }
        assert_eq!(
            aggregator.aggregate().keyboard.keys(),
            &[KeyUsage(0x04), KeyUsage(0x05)]
        );

        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(7),
                interface_id: InterfaceId(1),
                keyboard: Some(KeyboardFrame::new(ModifierState::empty())),
                mouse: None,
                consumer: None,
            })
            .unwrap();
        assert_eq!(aggregator.aggregate().keyboard.keys(), &[KeyUsage(0x05)]);

        assert!(aggregator.remove_interface(InterfaceId(1)));
        assert_eq!(aggregator.aggregate().keyboard.keys(), &[KeyUsage(0x05)]);
        assert!(aggregator.remove_device(DeviceId(7)));
        assert!(aggregator.aggregate().keyboard.keys().is_empty());
    }
}
