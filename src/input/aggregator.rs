use crate::ids::DeviceId;

use super::{
    ConsumerState, InputError, MouseButtons, PhysicalInputState, PhysicalKeyboardState,
    StandardInputFrame,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct DeviceInputState {
    device_id: DeviceId,
    keyboard: PhysicalKeyboardState,
    mouse_buttons: MouseButtons,
    consumer: ConsumerState,
    generation: u32,
}

impl DeviceInputState {
    const fn new(device_id: DeviceId, generation: u32) -> Self {
        Self {
            device_id,
            keyboard: PhysicalKeyboardState::new(),
            mouse_buttons: MouseButtons::empty(),
            consumer: ConsumerState::new(),
            generation,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputAggregator<const DEVICES: usize> {
    devices: [Option<DeviceInputState>; DEVICES],
    aggregate: PhysicalInputState,
    next_generation: u32,
}

impl<const DEVICES: usize> InputAggregator<DEVICES> {
    pub const fn new() -> Self {
        Self {
            devices: [const { None }; DEVICES],
            aggregate: PhysicalInputState::new(),
            next_generation: 1,
        }
    }

    pub const fn aggregate(&self) -> &PhysicalInputState {
        &self.aggregate
    }

    pub fn apply_frame(&mut self, frame: &StandardInputFrame) -> Result<(), InputError> {
        let mut devices = self.devices.clone();
        let index = match device_index(&devices, frame.device_id) {
            Some(index) => index,
            None => self
                .devices
                .iter()
                .position(Option::is_none)
                .ok_or(InputError::DeviceCapacity)?,
        };

        let generation = self.next_generation;
        let entry =
            devices[index].get_or_insert(DeviceInputState::new(frame.device_id, generation));
        entry.generation = generation;

        if let Some(keyboard) = &frame.keyboard {
            entry.keyboard.replace_with_frame(keyboard)?;
        }
        if let Some(mouse) = frame.mouse {
            entry.mouse_buttons = mouse.buttons;
        }
        if let Some(consumer) = frame.consumer {
            entry.consumer.active = consumer.active;
        }

        let aggregate = rebuild_aggregate(&devices)?;
        self.devices = devices;
        self.aggregate = aggregate;
        self.next_generation = self.next_generation.wrapping_add(1);
        Ok(())
    }

    pub fn remove_device(&mut self, device_id: DeviceId) -> bool {
        let Some(index) = self.device_index(device_id) else {
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

    fn device_index(&self, device_id: DeviceId) -> Option<usize> {
        self.devices
            .iter()
            .position(|entry| matches!(entry, Some(entry) if entry.device_id == device_id))
    }
}

fn device_index(devices: &[Option<DeviceInputState>], device_id: DeviceId) -> Option<usize> {
    devices
        .iter()
        .position(|entry| matches!(entry, Some(entry) if entry.device_id == device_id))
}

fn rebuild_aggregate(
    devices: &[Option<DeviceInputState>],
) -> Result<PhysicalInputState, InputError> {
    let mut aggregate = PhysicalInputState::new();
    let mut newest_consumer_generation = 0u32;
    for entry in devices.iter().filter_map(|entry| entry.as_ref()) {
        aggregate.keyboard.modifiers |= entry.keyboard.modifiers;
        for key in entry.keyboard.keys().iter().copied() {
            aggregate.keyboard.press_key(key)?;
        }
        aggregate.mouse.buttons = MouseButtons::from_bits_truncate(
            aggregate.mouse.buttons.bits() | entry.mouse_buttons.bits(),
        );
        if entry.consumer.active.is_some() && entry.generation >= newest_consumer_generation {
            aggregate.consumer = entry.consumer;
            newest_consumer_generation = entry.generation;
        }
    }
    Ok(aggregate)
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
    fn capacity_error_does_not_partially_update_device_or_aggregate_state() {
        let mut aggregator = InputAggregator::<2>::new();
        let mut first = KeyboardFrame::new(ModifierState::empty());
        for usage in 4..=23 {
            first.push_key(KeyUsage(usage)).unwrap();
        }
        aggregator
            .apply_frame(&StandardInputFrame {
                device_id: DeviceId(1),
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
                keyboard: Some(second),
                mouse: None,
                consumer: None,
            }),
            Err(InputError::KeyCapacity)
        );
        assert_eq!(aggregator, before);
    }
}
