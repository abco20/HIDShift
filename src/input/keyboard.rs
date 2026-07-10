use super::InputError;

pub const PHYSICAL_KEYBOARD_KEY_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct KeyUsage(pub u8);

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
    pub struct ModifierState: u8 {
        const LEFT_CTRL = 1 << 0;
        const LEFT_SHIFT = 1 << 1;
        const LEFT_ALT = 1 << 2;
        const LEFT_GUI = 1 << 3;
        const RIGHT_CTRL = 1 << 4;
        const RIGHT_SHIFT = 1 << 5;
        const RIGHT_ALT = 1 << 6;
        const RIGHT_GUI = 1 << 7;
    }
}

impl ModifierState {
    pub fn set_modifier(&mut self, modifier: Modifier, pressed: bool) {
        self.set(modifier.flag(), pressed);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Modifier {
    LeftCtrl,
    LeftShift,
    LeftAlt,
    LeftGui,
    RightCtrl,
    RightShift,
    RightAlt,
    RightGui,
}

impl Modifier {
    pub const fn flag(self) -> ModifierState {
        match self {
            Self::LeftCtrl => ModifierState::LEFT_CTRL,
            Self::LeftShift => ModifierState::LEFT_SHIFT,
            Self::LeftAlt => ModifierState::LEFT_ALT,
            Self::LeftGui => ModifierState::LEFT_GUI,
            Self::RightCtrl => ModifierState::RIGHT_CTRL,
            Self::RightShift => ModifierState::RIGHT_SHIFT,
            Self::RightAlt => ModifierState::RIGHT_ALT,
            Self::RightGui => ModifierState::RIGHT_GUI,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardFrame {
    pub modifiers: ModifierState,
    keys_down: heapless::Vec<KeyUsage, PHYSICAL_KEYBOARD_KEY_CAPACITY>,
}

impl KeyboardFrame {
    pub const fn new(modifiers: ModifierState) -> Self {
        Self {
            modifiers,
            keys_down: heapless::Vec::new(),
        }
    }

    pub fn push_key(&mut self, key: KeyUsage) -> Result<(), InputError> {
        if self.keys_down.contains(&key) {
            return Ok(());
        }
        self.keys_down
            .push(key)
            .map_err(|_| InputError::KeyCapacity)
    }

    pub fn keys_down(&self) -> &[KeyUsage] {
        &self.keys_down
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PhysicalKeyboardState {
    pub modifiers: ModifierState,
    keys: heapless::Vec<KeyUsage, PHYSICAL_KEYBOARD_KEY_CAPACITY>,
}

impl PhysicalKeyboardState {
    pub const fn new() -> Self {
        Self {
            modifiers: ModifierState::empty(),
            keys: heapless::Vec::new(),
        }
    }

    pub fn apply_frame(
        &mut self,
        frame: &KeyboardFrame,
        suppression: &mut KeyboardSuppression,
    ) -> Result<(), InputError> {
        self.modifiers = frame.modifiers;

        let mut index = 0;
        while index < self.keys.len() {
            let key = self.keys[index];
            if frame.keys_down().contains(&key) {
                index += 1;
            } else {
                self.keys.remove(index);
                suppression.release_key(key);
            }
        }

        for key in frame.keys_down().iter().copied() {
            self.press_key(key)?;
        }

        suppression.retain_pressed_by(self);
        Ok(())
    }

    pub fn replace_with_frame(&mut self, frame: &KeyboardFrame) -> Result<(), InputError> {
        self.modifiers = frame.modifiers;
        self.keys.clear();
        for key in frame.keys_down().iter().copied() {
            self.keys.push(key).map_err(|_| InputError::KeyCapacity)?;
        }
        Ok(())
    }

    pub fn press_key(&mut self, key: KeyUsage) -> Result<(), InputError> {
        if self.contains_key(key) {
            return Ok(());
        }
        self.keys.push(key).map_err(|_| InputError::KeyCapacity)
    }

    pub fn release_key(&mut self, key: KeyUsage, suppression: &mut KeyboardSuppression) {
        remove_key(&mut self.keys, key);
        suppression.release_key(key);
    }

    pub fn set_modifier(&mut self, modifier: Modifier, pressed: bool) {
        self.modifiers.set_modifier(modifier, pressed);
    }

    pub fn keys(&self) -> &[KeyUsage] {
        &self.keys
    }

    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    pub fn contains_key(&self, key: KeyUsage) -> bool {
        self.keys.contains(&key)
    }

    pub fn visible_against(&self, suppression: &KeyboardSuppression) -> VisibleKeyboardState {
        let mut visible = VisibleKeyboardState {
            modifiers: self.modifiers & !suppression.modifiers,
            keys: heapless::Vec::new(),
            truncated: false,
        };

        for key in self.keys.iter().copied() {
            if suppression.contains_key(key) {
                continue;
            }
            if visible.keys.push(key).is_err() {
                visible.truncated = true;
            }
        }

        visible
    }

    pub fn to_frame(&self) -> Result<KeyboardFrame, InputError> {
        let mut frame = KeyboardFrame::new(self.modifiers);
        for key in self.keys.iter().copied() {
            frame.push_key(key)?;
        }
        Ok(frame)
    }
}

impl Default for PhysicalKeyboardState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeyboardSuppression {
    modifiers: ModifierState,
    keys: heapless::Vec<KeyUsage, PHYSICAL_KEYBOARD_KEY_CAPACITY>,
}

impl KeyboardSuppression {
    pub const fn new() -> Self {
        Self {
            modifiers: ModifierState::empty(),
            keys: heapless::Vec::new(),
        }
    }

    pub fn capture_from(&mut self, state: &PhysicalKeyboardState) -> Result<(), InputError> {
        self.modifiers = state.modifiers;
        self.keys.clear();
        for key in state.keys().iter().copied() {
            self.keys.push(key).map_err(|_| InputError::KeyCapacity)?;
        }
        Ok(())
    }

    pub fn contains_key(&self, key: KeyUsage) -> bool {
        self.keys.contains(&key)
    }

    fn release_key(&mut self, key: KeyUsage) {
        remove_key(&mut self.keys, key);
    }

    fn retain_pressed_by(&mut self, state: &PhysicalKeyboardState) {
        let mut index = 0;
        while index < self.keys.len() {
            if state.contains_key(self.keys[index]) {
                index += 1;
            } else {
                self.keys.remove(index);
            }
        }
        self.modifiers &= state.modifiers;
    }
}

impl Default for KeyboardSuppression {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VisibleKeyboardState {
    pub modifiers: ModifierState,
    keys: heapless::Vec<KeyUsage, PHYSICAL_KEYBOARD_KEY_CAPACITY>,
    truncated: bool,
}

impl VisibleKeyboardState {
    pub fn is_empty(&self) -> bool {
        self.modifiers.is_empty() && self.keys.is_empty()
    }

    pub fn keys(&self) -> &[KeyUsage] {
        &self.keys
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

fn remove_key<const N: usize>(keys: &mut heapless::Vec<KeyUsage, N>, key: KeyUsage) {
    if let Some(index) = keys.iter().position(|candidate| *candidate == key) {
        keys.remove(index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_keyboard_tracks_non_modifier_keys_in_press_order() {
        let mut keyboard = PhysicalKeyboardState::new();

        keyboard.press_key(KeyUsage(0x06)).unwrap();
        keyboard.press_key(KeyUsage(0x04)).unwrap();
        keyboard.press_key(KeyUsage(0x05)).unwrap();
        keyboard.press_key(KeyUsage(0x04)).unwrap();

        assert_eq!(
            keyboard.keys(),
            &[KeyUsage(0x06), KeyUsage(0x04), KeyUsage(0x05)]
        );
    }

    #[test]
    fn physical_keyboard_reports_capacity_without_losing_existing_keys() {
        let mut keyboard = PhysicalKeyboardState::new();

        for usage in 0x04..0x24 {
            keyboard.press_key(KeyUsage(usage)).unwrap();
        }

        assert_eq!(
            keyboard.press_key(KeyUsage(0x24)),
            Err(InputError::KeyCapacity)
        );
        assert_eq!(keyboard.key_count(), PHYSICAL_KEYBOARD_KEY_CAPACITY);
        assert!(keyboard.contains_key(KeyUsage(0x04)));
        assert!(!keyboard.contains_key(KeyUsage(0x24)));
    }

    #[test]
    fn frame_update_preserves_existing_press_order_and_releases_missing_keys() {
        let mut keyboard = PhysicalKeyboardState::new();
        let mut suppression = KeyboardSuppression::new();
        keyboard.press_key(KeyUsage(0x06)).unwrap();
        keyboard.press_key(KeyUsage(0x04)).unwrap();
        keyboard.press_key(KeyUsage(0x05)).unwrap();

        let mut frame = KeyboardFrame::new(ModifierState::LEFT_SHIFT);
        frame.push_key(KeyUsage(0x05)).unwrap();
        frame.push_key(KeyUsage(0x06)).unwrap();
        frame.push_key(KeyUsage(0x07)).unwrap();

        keyboard.apply_frame(&frame, &mut suppression).unwrap();

        assert_eq!(
            keyboard.keys(),
            &[KeyUsage(0x06), KeyUsage(0x05), KeyUsage(0x07)]
        );
        assert_eq!(keyboard.modifiers, ModifierState::LEFT_SHIFT);
    }

    #[test]
    fn keyboard_visible_state_excludes_suppressed_keys_until_release() {
        let mut physical = PhysicalKeyboardState::new();
        physical.press_key(KeyUsage(0x04)).unwrap();
        physical.press_key(KeyUsage(0x05)).unwrap();
        physical.press_key(KeyUsage(0x06)).unwrap();

        let mut suppression = KeyboardSuppression::new();
        suppression.capture_from(&physical).unwrap();

        let visible = physical.visible_against(&suppression);
        assert!(visible.is_empty());

        physical.release_key(KeyUsage(0x05), &mut suppression);
        physical.press_key(KeyUsage(0x07)).unwrap();

        let visible = physical.visible_against(&suppression);
        assert_eq!(visible.keys(), &[KeyUsage(0x07)]);
        assert!(!suppression.contains_key(KeyUsage(0x05)));
    }

    #[test]
    fn modifier_state_uses_ble_boot_modifier_bits() {
        let mut modifiers = ModifierState::empty();

        modifiers.set_modifier(Modifier::LeftCtrl, true);
        modifiers.set_modifier(Modifier::RightGui, true);

        assert_eq!(modifiers.bits(), 0b1000_0001);
    }
}
