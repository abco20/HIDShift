use crate::input::{MouseButtons, MouseMovement, MouseReport};

pub const MOUSE_REPORT_ID: u8 = 2;
pub const MOUSE_REPORT_LEN: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleMouseReport {
    bytes: [u8; MOUSE_REPORT_LEN],
}

impl BleMouseReport {
    pub const fn from_mouse(report: MouseReport) -> Self {
        Self {
            bytes: [
                report.buttons.bits(),
                report.x as u8,
                report.y as u8,
                report.wheel as u8,
                report.pan as u8,
            ],
        }
    }

    pub fn from_frame(buttons: MouseButtons, movement: MouseMovement) -> Self {
        Self {
            bytes: [
                buttons.bits(),
                clamp_i16_to_i8(movement.x) as u8,
                clamp_i16_to_i8(movement.y) as u8,
                movement.wheel as u8,
                movement.pan as u8,
            ],
        }
    }

    pub const fn release_buttons() -> Self {
        Self {
            bytes: [0; MOUSE_REPORT_LEN],
        }
    }

    pub const fn as_bytes(&self) -> &[u8; MOUSE_REPORT_LEN] {
        &self.bytes
    }
}

const fn clamp_i16_to_i8(value: i16) -> i8 {
    if value > i8::MAX as i16 {
        i8::MAX
    } else if value < i8::MIN as i16 {
        i8::MIN
    } else {
        value as i8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::MouseButton;

    #[test]
    fn mouse_report_uses_expected_ble_byte_layout() {
        let mut buttons = MouseButtons::empty();
        buttons.set(MouseButton::Left, true);
        buttons.set(MouseButton::Forward, true);

        let report = BleMouseReport::from_mouse(MouseReport {
            buttons,
            x: -3,
            y: 4,
            wheel: 1,
            pan: -1,
        });

        assert_eq!(report.as_bytes(), &[0b0001_0001, 253, 4, 1, 255]);
    }

    #[test]
    fn mouse_frame_clamps_large_relative_movement() {
        let report = BleMouseReport::from_frame(
            MouseButtons::LEFT,
            MouseMovement {
                x: 500,
                y: -500,
                wheel: 2,
                pan: -2,
            },
        );

        assert_eq!(report.as_bytes(), &[1, 127, 128, 2, 254]);
    }
}
