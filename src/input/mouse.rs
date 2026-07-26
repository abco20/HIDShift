#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PhysicalMouseState {
    pub buttons: MouseButtons,
}

impl PhysicalMouseState {
    pub const fn new() -> Self {
        Self {
            buttons: MouseButtons::empty(),
        }
    }

    pub fn apply_frame(&mut self, frame: MouseFrame) {
        self.buttons = frame.buttons;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseFrame {
    pub buttons: MouseButtons,
    pub movement: MouseMovement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseMovement {
    pub x: i16,
    pub y: i16,
    pub wheel: i8,
    pub pan: i8,
}

impl MouseMovement {
    pub const fn neutral() -> Self {
        Self {
            x: 0,
            y: 0,
            wheel: 0,
            pan: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MouseInputReport {
    pub buttons: MouseButtons,
    pub x: i8,
    pub y: i8,
    pub wheel: i8,
    pub pan: i8,
}

impl MouseInputReport {
    pub const fn neutral() -> Self {
        Self {
            buttons: MouseButtons::empty(),
            x: 0,
            y: 0,
            wheel: 0,
            pan: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MouseButtons(u8);

impl MouseButtons {
    pub const LEFT: Self = Self(1 << 0);
    pub const RIGHT: Self = Self(1 << 1);
    pub const MIDDLE: Self = Self(1 << 2);
    pub const BACK: Self = Self(1 << 3);
    pub const FORWARD: Self = Self(1 << 4);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits_truncate(bits: u8) -> Self {
        Self(bits & 0b0001_1111)
    }

    pub const fn bits(self) -> u8 {
        self.0
    }

    pub const fn contains(self, button: MouseButton) -> bool {
        self.0 & button.mask() != 0
    }

    pub const fn without(self, suppressed: Self) -> Self {
        Self(self.0 & !suppressed.0)
    }

    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    pub fn set(&mut self, button: MouseButton, pressed: bool) {
        if pressed {
            self.0 |= button.mask();
        } else {
            self.0 &= !button.mask();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl MouseButton {
    pub const fn mask(self) -> u8 {
        match self {
            Self::Left => MouseButtons::LEFT.0,
            Self::Right => MouseButtons::RIGHT.0,
            Self::Middle => MouseButtons::MIDDLE.0,
            Self::Back => MouseButtons::BACK.0,
            Self::Forward => MouseButtons::FORWARD.0,
        }
    }
}
