use heapless::Vec;

use crate::input::{ConsumerUsage, KeyUsage, KeyboardFrame, StandardInputFrame};
use crate::settings::InputSettings;

pub const INPUT_PROFILE_CAPACITY: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InputProfileId(u8);

impl InputProfileId {
    pub const fn new(value: u8) -> Option<Self> {
        if value >= 1 && value <= INPUT_PROFILE_CAPACITY as u8 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct InputIdentity {
    pub vendor_id: u16,
    pub product_id: u16,
    /// Hash of the serial string, or of the physical port path when no serial exists.
    pub instance_hash: u64,
}

impl InputIdentity {
    pub fn from_serial(vendor_id: u16, product_id: u16, serial: &[u8]) -> Option<Self> {
        if serial.is_empty() {
            return None;
        }
        Some(Self {
            vendor_id,
            product_id,
            instance_hash: identity_hash(0x53, serial),
        })
    }

    pub fn from_serial_utf16(vendor_id: u16, product_id: u16, serial: &[u16]) -> Option<Self> {
        if serial.is_empty() {
            return None;
        }
        let mut hash = identity_hash_start(0x53);
        for unit in serial {
            for byte in unit.to_le_bytes() {
                hash = identity_hash_byte(hash, byte);
            }
        }
        Some(Self {
            vendor_id,
            product_id,
            instance_hash: hash,
        })
    }

    pub fn from_port_path(vendor_id: u16, product_id: u16, port_path: &[u8]) -> Self {
        Self {
            vendor_id,
            product_id,
            instance_hash: identity_hash(0x50, port_path),
        }
    }
}

fn identity_hash(kind: u8, bytes: &[u8]) -> u64 {
    let mut hash = identity_hash_start(kind);
    for byte in bytes {
        hash = identity_hash_byte(hash, *byte);
    }
    hash
}

fn identity_hash_start(kind: u8) -> u64 {
    identity_hash_byte(0xcbf2_9ce4_8422_2325, kind)
}

fn identity_hash_byte(hash: u64, byte: u8) -> u64 {
    (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MouseScaleRemainders {
    x: i32,
    y: i32,
    wheel: i32,
    pan: i32,
}

impl MouseScaleRemainders {
    const ZERO: Self = Self {
        x: 0,
        y: 0,
        wheel: 0,
        pan: 0,
    };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputProfile {
    pub id: InputProfileId,
    pub identity: InputIdentity,
    pub connected: bool,
    pub last_seen: u64,
    pub settings: InputSettings,
    mouse_remainders: MouseScaleRemainders,
}

impl InputProfile {
    pub const fn restored(
        id: InputProfileId,
        identity: InputIdentity,
        last_seen: u64,
        settings: InputSettings,
    ) -> Self {
        Self {
            id,
            identity,
            connected: false,
            last_seen,
            settings,
            mouse_remainders: MouseScaleRemainders::ZERO,
        }
    }

    /// Applies device-owned transforms before input aggregation.
    pub fn transform(&mut self, frame: &mut StandardInputFrame) {
        if let Some(keyboard) = frame.keyboard.take() {
            let mut transformed = KeyboardFrame::new(keyboard.modifiers);
            for key in keyboard.keys_down() {
                let layout_key = match (self.settings.keyboard_layout, key.0) {
                    (1, 0x89) => 0x35,
                    (2, 0x35) => 0x89,
                    _ => key.0,
                };
                let mapped = if self.settings.remap_from_usage != 0
                    && layout_key == self.settings.remap_from_usage
                {
                    self.settings.remap_to_usage
                } else {
                    layout_key
                };
                let _ = transformed.push_key(KeyUsage(mapped));
            }
            frame.keyboard = Some(transformed);
        }
        if let Some(mouse) = frame.mouse.as_mut() {
            mouse.movement.x = scale_i16_with_remainder(
                mouse.movement.x,
                self.settings.mouse_sensitivity_percent,
                &mut self.mouse_remainders.x,
            );
            mouse.movement.y = scale_i16_with_remainder(
                mouse.movement.y,
                self.settings.mouse_sensitivity_percent,
                &mut self.mouse_remainders.y,
            );
            mouse.movement.wheel = scale_i8_with_remainder(
                mouse.movement.wheel,
                self.settings.scroll_multiplier_percent,
                &mut self.mouse_remainders.wheel,
            );
            mouse.movement.pan = scale_i8_with_remainder(
                mouse.movement.pan,
                self.settings.scroll_multiplier_percent,
                &mut self.mouse_remainders.pan,
            );
        }
        if let Some(consumer) = frame.consumer.as_mut()
            && let Some(active) = consumer.active
            && self.settings.consumer_from_usage != 0
            && active.0 == self.settings.consumer_from_usage
        {
            consumer.active = Some(ConsumerUsage(self.settings.consumer_to_usage));
        }
    }

    pub fn reset_transform_state(&mut self) {
        self.mouse_remainders = MouseScaleRemainders::ZERO;
    }
}

fn scale_i16_with_remainder(value: i16, percent: u16, remainder: &mut i32) -> i16 {
    let scaled = i32::from(value) * i32::from(percent) + *remainder;
    let output = scaled / 100;
    *remainder = scaled % 100;
    output.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

fn scale_i8_with_remainder(value: i8, percent: u16, remainder: &mut i32) -> i8 {
    let scaled = i32::from(value) * i32::from(percent) + *remainder;
    let output = scaled / 100;
    *remainder = scaled % 100;
    output.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputProfileError {
    RegistryFullWithConnectedProfiles,
    UnknownProfile,
    DuplicateProfileId,
    DuplicateIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputProfileRegistry {
    profiles: Vec<InputProfile, INPUT_PROFILE_CAPACITY>,
    next_sequence: u64,
}

impl Default for InputProfileRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl InputProfileRegistry {
    pub const fn new() -> Self {
        Self {
            profiles: Vec::new(),
            next_sequence: 1,
        }
    }

    pub fn profiles(&self) -> &[InputProfile] {
        &self.profiles
    }

    pub fn restore(&mut self, profile: InputProfile) -> Result<(), InputProfileError> {
        if self.profiles.iter().any(|item| item.id == profile.id) {
            return Err(InputProfileError::DuplicateProfileId);
        }
        if self
            .profiles
            .iter()
            .any(|item| item.identity == profile.identity)
        {
            return Err(InputProfileError::DuplicateIdentity);
        }
        self.next_sequence = self.next_sequence.max(profile.last_seen.wrapping_add(1));
        self.profiles
            .push(profile)
            .map_err(|_| InputProfileError::RegistryFullWithConnectedProfiles)
    }

    pub fn observe(
        &mut self,
        identity: InputIdentity,
        _now: u64,
    ) -> Result<InputProfileId, InputProfileError> {
        let sequence = self.take_sequence();
        if let Some(profile) = self
            .profiles
            .iter_mut()
            .find(|item| item.identity == identity)
        {
            profile.connected = true;
            profile.last_seen = sequence;
            return Ok(profile.id);
        }
        if self.profiles.is_full() {
            let removable = self
                .profiles
                .iter()
                .enumerate()
                .filter(|(_, profile)| !profile.connected)
                .min_by_key(|(_, profile)| profile.last_seen)
                .map(|(index, _)| index)
                .ok_or(InputProfileError::RegistryFullWithConnectedProfiles)?;
            let id = self.profiles[removable].id;
            self.profiles[removable] =
                InputProfile::restored(id, identity, sequence, InputSettings::DEFAULT);
            self.profiles[removable].connected = true;
            return Ok(id);
        }
        let id = (1..=INPUT_PROFILE_CAPACITY as u8)
            .find_map(|candidate| {
                let id = InputProfileId::new(candidate)?;
                (!self.profiles.iter().any(|profile| profile.id == id)).then_some(id)
            })
            .ok_or(InputProfileError::RegistryFullWithConnectedProfiles)?;
        let mut profile = InputProfile::restored(id, identity, sequence, InputSettings::DEFAULT);
        profile.connected = true;
        self.profiles
            .push(profile)
            .map_err(|_| InputProfileError::RegistryFullWithConnectedProfiles)?;
        Ok(id)
    }

    pub fn disconnect(&mut self, id: InputProfileId, _now: u64) -> Result<(), InputProfileError> {
        let sequence = self.take_sequence();
        let profile = self.get_mut(id).ok_or(InputProfileError::UnknownProfile)?;
        profile.connected = false;
        profile.last_seen = sequence;
        profile.reset_transform_state();
        Ok(())
    }

    pub fn get(&self, id: InputProfileId) -> Option<&InputProfile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub fn get_mut(&mut self, id: InputProfileId) -> Option<&mut InputProfile> {
        self.profiles.iter_mut().find(|profile| profile.id == id)
    }

    fn take_sequence(&mut self) -> u64 {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        sequence
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{DeviceId, InterfaceId};
    use crate::input::{MouseButtons, MouseFrame, MouseMovement};

    fn identity(value: u16) -> InputIdentity {
        InputIdentity::from_port_path(value, value, &[value as u8])
    }

    #[test]
    fn serial_and_path_identity_are_stable_and_distinct() {
        let serial = InputIdentity::from_serial(1, 2, b"abc").unwrap();
        assert_eq!(serial, InputIdentity::from_serial(1, 2, b"abc").unwrap());
        assert_ne!(serial, InputIdentity::from_port_path(1, 2, b"abc"));
        assert_eq!(InputIdentity::from_serial(1, 2, b""), None);
    }

    #[test]
    fn oldest_disconnected_profile_is_replaced_even_when_customized() {
        let mut registry = InputProfileRegistry::new();
        let mut oldest = None;
        for value in 1..=8 {
            let id = registry.observe(identity(value), value as u64).unwrap();
            registry.get_mut(id).unwrap().settings.keyboard_layout = 2;
            registry.disconnect(id, value as u64).unwrap();
            oldest.get_or_insert(id);
        }
        let replacement = registry.observe(identity(9), 9).unwrap();
        assert_eq!(replacement, oldest.unwrap());
        assert_eq!(
            registry.get(replacement).unwrap().settings,
            InputSettings::DEFAULT
        );
    }

    #[test]
    fn connected_profiles_are_not_replaced() {
        let mut registry = InputProfileRegistry::new();
        for value in 1..=8 {
            registry.observe(identity(value), value as u64).unwrap();
        }
        assert_eq!(
            registry.observe(identity(9), 9),
            Err(InputProfileError::RegistryFullWithConnectedProfiles)
        );
    }

    #[test]
    fn reconnect_restores_the_same_profile_and_settings() {
        let mut registry = InputProfileRegistry::new();
        let id = registry.observe(identity(1), 1).unwrap();
        registry.get_mut(id).unwrap().settings.remap_from_usage = 4;
        registry.get_mut(id).unwrap().settings.remap_to_usage = 5;
        registry.disconnect(id, 2).unwrap();

        assert_eq!(registry.observe(identity(1), 3), Ok(id));
        assert_eq!(registry.get(id).unwrap().settings.remap_to_usage, 5);
    }

    #[test]
    fn transform_keeps_fractional_mouse_movement_per_axis() {
        let id = InputProfileId::new(1).unwrap();
        let mut profile = InputProfile::restored(id, identity(1), 0, InputSettings::DEFAULT);
        profile.settings.mouse_sensitivity_percent = 50;
        let mut frame = StandardInputFrame {
            device_id: DeviceId(1),
            interface_id: InterfaceId(1),
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
        };
        profile.transform(&mut frame);
        assert_eq!(frame.mouse.unwrap().movement.x, 0);
        frame.mouse.as_mut().unwrap().movement.x = 1;
        frame.mouse.as_mut().unwrap().movement.y = -1;
        profile.transform(&mut frame);
        assert_eq!(frame.mouse.unwrap().movement.x, 1);
        assert_eq!(frame.mouse.unwrap().movement.y, -1);
    }
}
