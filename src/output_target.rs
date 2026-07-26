use crate::ids::{HOST_SLOT_COUNT, HostId};

pub const MIRROR_PORT_PATH_MAX_LEN: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTarget {
    Wired,
    Ble(HostId),
}

impl OutputTarget {
    pub const fn validate(self) -> Result<Self, OutputTargetError> {
        match self {
            Self::Wired => Ok(self),
            Self::Ble(host) if host.0 >= 1 && (host.0 as usize) <= HOST_SLOT_COUNT => Ok(self),
            Self::Ble(_) => Err(OutputTargetError::InvalidBleHost),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTargetError {
    InvalidBleHost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputTargetAvailability {
    Ready,
    ConnectedNotReady,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleTargetReadinessPolicy {
    KeyboardRequired,
    AllStandardReports,
    AnyReport,
}

impl BleTargetReadinessPolicy {
    pub const fn is_ready(self, keyboard: bool, mouse: bool, consumer: bool) -> bool {
        match self {
            Self::KeyboardRequired => keyboard,
            Self::AllStandardReports => keyboard && mouse && consumer,
            Self::AnyReport => keyboard || mouse || consumer,
        }
    }
}

pub const BLE_TARGET_READINESS_POLICY: BleTargetReadinessPolicy =
    BleTargetReadinessPolicy::KeyboardRequired;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OutputTargetState {
    pub selected: OutputTarget,
    pub active: Option<OutputTarget>,
    pub availability: OutputTargetAvailability,
    pub transition_operation_id: u32,
}

impl OutputTargetState {
    pub const fn new() -> Self {
        Self {
            selected: OutputTarget::Wired,
            active: None,
            availability: OutputTargetAvailability::Unavailable,
            transition_operation_id: 0,
        }
    }

    pub fn select(&mut self, target: OutputTarget) -> Result<u32, OutputTargetError> {
        target.validate()?;
        self.selected = target;
        self.active = None;
        self.availability = OutputTargetAvailability::Unavailable;
        self.transition_operation_id = self.transition_operation_id.wrapping_add(1);
        Ok(self.transition_operation_id)
    }

    pub fn set_availability(&mut self, availability: OutputTargetAvailability) {
        self.availability = availability;
        self.active = match availability {
            OutputTargetAvailability::Ready => Some(self.selected),
            OutputTargetAvailability::ConnectedNotReady | OutputTargetAvailability::Unavailable => {
                None
            }
        };
    }

    pub fn begin_transition(&mut self) -> u32 {
        self.active = None;
        self.availability = OutputTargetAvailability::ConnectedNotReady;
        self.transition_operation_id = self.transition_operation_id.wrapping_add(1);
        self.transition_operation_id
    }
}

impl Default for OutputTargetState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorCandidateId(pub u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorConfiguration {
    Disabled,
    Selected(MirrorCandidateId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbPresentation {
    Fallback,
    Mirror(MirrorCandidateId),
}

pub const fn effective_presentation(
    output_target: OutputTarget,
    mirror: MirrorConfiguration,
    mirror_available: bool,
) -> UsbPresentation {
    match (output_target, mirror, mirror_available) {
        (OutputTarget::Wired, MirrorConfiguration::Selected(candidate), true) => {
            UsbPresentation::Mirror(candidate)
        }
        _ => UsbPresentation::Fallback,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredOutputTarget {
    Wired,
    Ble(crate::ids::HostSlot),
}

impl StoredOutputTarget {
    pub const fn as_output_target(self) -> OutputTarget {
        match self {
            Self::Wired => OutputTarget::Wired,
            Self::Ble(slot) => OutputTarget::Ble(HostId(slot.get())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorStableId {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_hash: Option<u32>,
    pub descriptor_hash: u32,
    port_path_len: u8,
    port_path: [u8; MIRROR_PORT_PATH_MAX_LEN],
}

impl MirrorStableId {
    pub fn new(
        vendor_id: u16,
        product_id: u16,
        serial_hash: Option<u32>,
        descriptor_hash: u32,
        port_path: &[u8],
    ) -> Result<Self, MirrorStableIdError> {
        if port_path.len() > MIRROR_PORT_PATH_MAX_LEN {
            return Err(MirrorStableIdError::PortPathTooLong);
        }
        if serial_hash.is_none() && port_path.is_empty() {
            return Err(MirrorStableIdError::MissingPhysicalLocation);
        }

        let mut stored_path = [0; MIRROR_PORT_PATH_MAX_LEN];
        stored_path[..port_path.len()].copy_from_slice(port_path);
        Ok(Self {
            vendor_id,
            product_id,
            serial_hash,
            descriptor_hash,
            port_path_len: port_path.len() as u8,
            port_path: stored_path,
        })
    }

    pub const fn synthetic(descriptor_hash: u32) -> Self {
        Self {
            vendor_id: 0xcafe,
            product_id: 0,
            serial_hash: Some(descriptor_hash),
            descriptor_hash,
            port_path_len: 0,
            port_path: [0; MIRROR_PORT_PATH_MAX_LEN],
        }
    }

    pub fn port_path(&self) -> &[u8] {
        &self.port_path[..usize::from(self.port_path_len)]
    }

    pub const fn port_path_len(&self) -> u8 {
        self.port_path_len
    }

    pub const fn port_path_bytes(&self) -> &[u8; MIRROR_PORT_PATH_MAX_LEN] {
        &self.port_path
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorStableIdError {
    MissingPhysicalLocation,
    PortPathTooLong,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredMirrorTarget(pub MirrorStableId);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredPresentationConfig {
    pub output_target: StoredOutputTarget,
    pub mirror_target: Option<StoredMirrorTarget>,
}

impl StoredPresentationConfig {
    pub const DEFAULT: Self = Self {
        output_target: StoredOutputTarget::Wired,
        mirror_target: None,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::HostSlot;

    #[test]
    fn usb_is_a_distinct_target_and_ble_slots_validate() {
        assert_eq!(OutputTarget::Wired.validate(), Ok(OutputTarget::Wired));
        for id in 1..=4 {
            assert_eq!(
                OutputTarget::Ble(HostId(id)).validate(),
                Ok(OutputTarget::Ble(HostId(id)))
            );
        }
        assert_eq!(
            OutputTarget::Ble(HostId(0)).validate(),
            Err(OutputTargetError::InvalidBleHost)
        );
        assert_eq!(
            OutputTarget::Ble(HostId(5)).validate(),
            Err(OutputTargetError::InvalidBleHost)
        );
    }

    #[test]
    fn selection_never_fails_over_and_only_ready_becomes_active() {
        let mut state = OutputTargetState::new();
        state.set_availability(OutputTargetAvailability::Ready);
        assert_eq!(state.active, Some(OutputTarget::Wired));

        state.select(OutputTarget::Ble(HostId(2))).unwrap();
        assert_eq!(state.selected, OutputTarget::Ble(HostId(2)));
        assert_eq!(state.active, None);
        state.set_availability(OutputTargetAvailability::Unavailable);
        assert_eq!(state.active, None);
        assert_eq!(state.selected, OutputTarget::Ble(HostId(2)));
    }

    #[test]
    fn ble_readiness_policy_requires_keyboard_without_requiring_every_report() {
        assert_eq!(
            BLE_TARGET_READINESS_POLICY,
            BleTargetReadinessPolicy::KeyboardRequired
        );
        assert!(!BLE_TARGET_READINESS_POLICY.is_ready(false, false, true));
        assert!(!BLE_TARGET_READINESS_POLICY.is_ready(false, true, true));
        assert!(BLE_TARGET_READINESS_POLICY.is_ready(true, false, false));
    }

    #[test]
    fn mirror_is_effective_only_for_available_wired_target() {
        let candidate = MirrorCandidateId(7);
        let selected = MirrorConfiguration::Selected(candidate);
        assert_eq!(
            effective_presentation(OutputTarget::Wired, selected, true),
            UsbPresentation::Mirror(candidate)
        );
        assert_eq!(
            effective_presentation(OutputTarget::Wired, selected, false),
            UsbPresentation::Fallback
        );
        assert_eq!(
            effective_presentation(OutputTarget::Ble(HostId(1)), selected, true),
            UsbPresentation::Fallback
        );
        assert_eq!(
            effective_presentation(OutputTarget::Wired, MirrorConfiguration::Disabled, true),
            UsbPresentation::Fallback
        );
    }

    #[test]
    fn stored_ble_target_round_trips_to_a_valid_runtime_target() {
        let slot = HostSlot::try_from(4).unwrap();
        assert_eq!(
            StoredOutputTarget::Ble(slot).as_output_target(),
            OutputTarget::Ble(HostId(4))
        );
    }

    #[test]
    fn stable_mirror_identity_requires_a_serial_or_physical_port_path() {
        assert_eq!(
            MirrorStableId::new(0x046d, 0xc547, None, 1, &[]),
            Err(MirrorStableIdError::MissingPhysicalLocation)
        );
        let identity =
            MirrorStableId::new(0x046d, 0xc547, None, 1, &[1, 3]).expect("valid port path");
        assert_eq!(identity.port_path(), &[1, 3]);

        let serial_identity =
            MirrorStableId::new(0x046d, 0xc547, Some(7), 1, &[]).expect("serial is stable");
        assert!(serial_identity.port_path().is_empty());
    }
}
