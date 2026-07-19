use crate::ids::{HOST_SLOT_COUNT, HostId};

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
pub struct StoredMirrorTarget(pub u8);

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
}
