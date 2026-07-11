use crate::ids::{DeviceId, InterfaceId};

use super::{MouseFrame, MouseMovement, StandardInputFrame};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingMovement {
    device_id: DeviceId,
    interface_id: InterfaceId,
    buttons: super::MouseButtons,
    x: i32,
    y: i32,
    wheel: i32,
    pan: i32,
    pending: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UsbMovementCoalescerStats {
    pub reports_coalesced: u32,
    pub movement_saturated: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbMovementCoalescerError {
    NotMovementOnly,
    InterfaceCapacity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsbMovementCoalescer<const INTERFACES: usize> {
    pending: [PendingMovement; INTERFACES],
    stats: UsbMovementCoalescerStats,
}

impl<const INTERFACES: usize> UsbMovementCoalescer<INTERFACES> {
    pub const fn new() -> Self {
        Self {
            pending: [PendingMovement {
                device_id: DeviceId(0),
                interface_id: InterfaceId(0),
                buttons: super::MouseButtons::empty(),
                x: 0,
                y: 0,
                wheel: 0,
                pan: 0,
                pending: false,
            }; INTERFACES],
            stats: UsbMovementCoalescerStats {
                reports_coalesced: 0,
                movement_saturated: 0,
            },
        }
    }

    pub fn push(&mut self, frame: &StandardInputFrame) -> Result<(), UsbMovementCoalescerError> {
        let Some(mouse) = frame.mouse else {
            return Err(UsbMovementCoalescerError::NotMovementOnly);
        };
        if frame.keyboard.is_some()
            || frame.consumer.is_some()
            || mouse.movement == MouseMovement::neutral()
        {
            return Err(UsbMovementCoalescerError::NotMovementOnly);
        }
        let index = self
            .pending
            .iter()
            .position(|pending| pending.pending && pending.interface_id == frame.interface_id)
            .or_else(|| self.pending.iter().position(|pending| !pending.pending))
            .ok_or(UsbMovementCoalescerError::InterfaceCapacity)?;
        let pending = &mut self.pending[index];
        if pending.pending {
            if pending.device_id != frame.device_id || pending.buttons != mouse.buttons {
                return Err(UsbMovementCoalescerError::NotMovementOnly);
            }
            self.stats.reports_coalesced = self.stats.reports_coalesced.saturating_add(1);
        } else {
            pending.device_id = frame.device_id;
            pending.interface_id = frame.interface_id;
            pending.buttons = mouse.buttons;
            pending.pending = true;
        }
        pending.x = add_axis(pending.x, i32::from(mouse.movement.x), &mut self.stats);
        pending.y = add_axis(pending.y, i32::from(mouse.movement.y), &mut self.stats);
        pending.wheel = add_axis(
            pending.wheel,
            i32::from(mouse.movement.wheel),
            &mut self.stats,
        );
        pending.pan = add_axis(pending.pan, i32::from(mouse.movement.pan), &mut self.stats);
        Ok(())
    }

    pub fn take_next(&mut self) -> Option<StandardInputFrame> {
        let pending = self.pending.iter_mut().find(|pending| pending.pending)?;
        let movement = MouseMovement {
            x: take_axis(&mut pending.x) as i16,
            y: take_axis(&mut pending.y) as i16,
            wheel: take_axis(&mut pending.wheel),
            pan: take_axis(&mut pending.pan),
        };
        let frame = StandardInputFrame {
            device_id: pending.device_id,
            interface_id: pending.interface_id,
            keyboard: None,
            mouse: Some(MouseFrame {
                buttons: pending.buttons,
                movement,
            }),
            consumer: None,
        };
        pending.pending =
            pending.x != 0 || pending.y != 0 || pending.wheel != 0 || pending.pan != 0;
        Some(frame)
    }

    pub const fn stats(&self) -> UsbMovementCoalescerStats {
        self.stats
    }
}

impl<const INTERFACES: usize> Default for UsbMovementCoalescer<INTERFACES> {
    fn default() -> Self {
        Self::new()
    }
}

fn add_axis(current: i32, delta: i32, stats: &mut UsbMovementCoalescerStats) -> i32 {
    current.checked_add(delta).unwrap_or_else(|| {
        stats.movement_saturated = stats.movement_saturated.saturating_add(1);
        current.saturating_add(delta)
    })
}

fn take_axis(value: &mut i32) -> i8 {
    let output = (*value).clamp(i8::MIN as i32, i8::MAX as i32) as i8;
    *value -= i32::from(output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movement(interface: u8, x: i16, buttons: super::super::MouseButtons) -> StandardInputFrame {
        StandardInputFrame {
            device_id: DeviceId(1),
            interface_id: InterfaceId(interface),
            keyboard: None,
            mouse: Some(MouseFrame {
                buttons,
                movement: MouseMovement {
                    x,
                    y: -x,
                    wheel: 1,
                    pan: -1,
                },
            }),
            consumer: None,
        }
    }

    #[test]
    fn full_runtime_queue_movement_is_coalesced_without_losing_sum() {
        let mut queue = UsbMovementCoalescer::<2>::new();
        for _ in 0..10 {
            queue
                .push(&movement(1, 30, super::super::MouseButtons::empty()))
                .unwrap();
        }
        let mut totals = [0i32; 4];
        while let Some(frame) = queue.take_next() {
            let movement = frame.mouse.unwrap().movement;
            totals[0] += i32::from(movement.x);
            totals[1] += i32::from(movement.y);
            totals[2] += i32::from(movement.wheel);
            totals[3] += i32::from(movement.pan);
        }
        assert_eq!(totals, [300, -300, 10, -10]);
        assert_eq!(queue.stats().reports_coalesced, 9);
    }

    #[test]
    fn button_or_non_mouse_frames_are_not_accepted_as_coalescible() {
        let mut queue = UsbMovementCoalescer::<1>::new();
        queue
            .push(&movement(1, 1, super::super::MouseButtons::empty()))
            .unwrap();
        assert_eq!(
            queue.push(&movement(1, 1, super::super::MouseButtons::LEFT)),
            Err(UsbMovementCoalescerError::NotMovementOnly)
        );

        let mut frame = movement(1, 1, super::super::MouseButtons::empty());
        frame.keyboard = Some(super::super::KeyboardFrame::new(
            super::super::ModifierState::empty(),
        ));
        assert_eq!(
            queue.push(&frame),
            Err(UsbMovementCoalescerError::NotMovementOnly)
        );
    }
}
