pub mod aggregator;
pub mod consumer;
pub mod keyboard;
pub mod leds;
pub mod mouse;
pub mod vendor;

pub use aggregator::InputAggregator;
pub use consumer::{ConsumerFrame, ConsumerState, ConsumerUsage};
pub use keyboard::{
    KeyUsage, KeyboardFrame, KeyboardSuppression, Modifier, ModifierState,
    PHYSICAL_KEYBOARD_KEY_CAPACITY, PhysicalKeyboardState, VisibleKeyboardState,
};
pub use leds::KeyboardLedState;
pub use mouse::{
    MouseButton, MouseButtons, MouseFrame, MouseMovement, MouseReport, PhysicalMouseState,
};
pub use vendor::{HidDirection, MAX_VENDOR_REPORT_SIZE, VendorHidFrame};

use crate::ids::DeviceId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalInputState {
    pub keyboard: PhysicalKeyboardState,
    pub mouse: PhysicalMouseState,
    pub consumer: ConsumerState,
}

impl PhysicalInputState {
    pub const fn new() -> Self {
        Self {
            keyboard: PhysicalKeyboardState::new(),
            mouse: PhysicalMouseState::new(),
            consumer: ConsumerState::new(),
        }
    }
}

impl Default for PhysicalInputState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputFrame {
    Standard(StandardInputFrame),
    Vendor(VendorHidFrame),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StandardInputFrame {
    pub device_id: DeviceId,
    pub keyboard: Option<KeyboardFrame>,
    pub mouse: Option<MouseFrame>,
    pub consumer: Option<ConsumerFrame>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputError {
    KeyCapacity,
    DeviceCapacity,
    VendorPayloadCapacity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputEvent {
    Keyboard(KeyboardEvent),
    Mouse(MouseReport),
    Consumer(ConsumerUsage),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyboardEvent {
    Press(KeyCode),
    Release(KeyCode),
    ModifierPress(Modifier),
    ModifierRelease(Modifier),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyCode {
    HidUsage(u8),
}
