use crate::bridge::{BridgeStatus, NotifyReason};
use crate::ids::{DeviceId, HostId, InterfaceId};
#[cfg(feature = "dual-s3-wired")]
use crate::interchip::{
    ActivateProfile, MirrorControlRequest, MirrorControlResponse, ProfileBegin, ProfileChunkData,
    ProfileTransferCommand, RawEndpointReport,
};
use crate::management::{ManagementDestination, ManagementResponse};
use crate::reports::BleHidReport;
#[cfg(feature = "dual-s3-wired")]
use crate::reports::StandardHidReport;
use crate::storage::{StoragePersistPriority, StorageState, StoredBond};
use crate::usb_hid::output::KeyboardLedOutputBytes;

use super::StatusSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
// Storage snapshots remain inline so the core does not require alloc.
#[allow(clippy::large_enum_variant)]
pub enum RuntimeCommand {
    BleCommand(BleTaskCommand),
    #[cfg(feature = "dual-s3-wired")]
    DeviceCommand(DeviceTaskCommand),
    UsbKeyboardLedWrite {
        interface_id: InterfaceId,
        device_id: DeviceId,
        bytes: KeyboardLedOutputBytes,
    },
    #[cfg(feature = "dual-s3-wired")]
    UsbMirrorEndpointOut {
        device_id: DeviceId,
        report: RawEndpointReport,
    },
    #[cfg(feature = "dual-s3-wired")]
    UsbMirrorControlRequest {
        device_id: DeviceId,
        request: MirrorControlRequest,
    },
    PersistStorage {
        state: StorageState,
        priority: StoragePersistPriority,
    },
    StatusChanged(StatusSnapshot),
    ManagementResponse {
        destination: ManagementDestination,
        response: ManagementResponse,
    },
    ApplyEffect(RuntimeEffect),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeEffect {
    SetLogLevel(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandClass {
    Critical,
    Realtime,
    BestEffort,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleCommandLane {
    Control,
    Notify,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleTaskCommand {
    Notify {
        host_id: HostId,
        report: BleHidReport,
        reason: NotifyReason,
    },
    AllowPairing {
        host_id: HostId,
    },
    RejectPairing {
        host_id: HostId,
    },
    ClearBond {
        host_id: HostId,
        bond: Option<StoredBond>,
    },
    ActivateInput {
        host_id: HostId,
    },
    ManagementResponse {
        host_id: HostId,
        response: ManagementResponse,
    },
    ManagementEvent {
        host_id: HostId,
        event: crate::management::ManagementEvent,
    },
}

#[cfg(feature = "dual-s3-wired")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceTaskCommand {
    StandardReport {
        report: StandardHidReport,
        reason: NotifyReason,
    },
    ReleaseAll,
    ActivateFallback {
        operation_id: u32,
    },
    ActivateMirror(ActivateProfile),
    ProfileBegin(ProfileBegin),
    ProfileChunk(ProfileChunkData),
    ProfileCommit {
        transfer_id: u32,
    },
    RawEndpointIn(RawEndpointReport),
    ControlResponse(MirrorControlResponse),
}

#[cfg(feature = "dual-s3-wired")]
impl DeviceTaskCommand {
    pub const fn class(self) -> CommandClass {
        match self {
            Self::StandardReport { reason, .. } => match reason {
                NotifyReason::Input => CommandClass::Realtime,
                NotifyReason::InputEdge
                | NotifyReason::InputRelease
                | NotifyReason::TargetSwitchRelease
                | NotifyReason::UsbDeviceRemovedRelease
                | NotifyReason::SafetyRelease => CommandClass::Critical,
            },
            Self::ReleaseAll | Self::ActivateFallback { .. } | Self::ActivateMirror(_) => {
                CommandClass::Critical
            }
            Self::ProfileBegin(_) | Self::ProfileChunk(_) | Self::ProfileCommit { .. } => {
                CommandClass::BestEffort
            }
            Self::RawEndpointIn(_) => CommandClass::Realtime,
            Self::ControlResponse(_) => CommandClass::Critical,
        }
    }
}

#[cfg(feature = "dual-s3-wired")]
impl From<ProfileTransferCommand> for DeviceTaskCommand {
    fn from(command: ProfileTransferCommand) -> Self {
        match command {
            ProfileTransferCommand::Begin(begin) => Self::ProfileBegin(begin),
            ProfileTransferCommand::Chunk(chunk) => Self::ProfileChunk(chunk),
            ProfileTransferCommand::Commit { transfer_id } => Self::ProfileCommit { transfer_id },
        }
    }
}

impl BleTaskCommand {
    /// Returns whether applying this command changes who may connect.
    ///
    /// Realtime notifications and ordinary GATT work must not restart an
    /// active advertiser. Restarting it disables and re-enables advertising
    /// through HCI, which can contend with the next connection event.
    pub const fn changes_advertising_policy(self) -> bool {
        matches!(
            self,
            Self::AllowPairing { .. } | Self::RejectPairing { .. } | Self::ClearBond { .. }
        )
    }

    pub const fn lane(self) -> BleCommandLane {
        match self {
            Self::Notify {
                reason: NotifyReason::Input,
                ..
            } => BleCommandLane::Notify,
            Self::Notify { .. } => BleCommandLane::Control,
            Self::AllowPairing { .. }
            | Self::RejectPairing { .. }
            | Self::ClearBond { .. }
            | Self::ActivateInput { .. }
            | Self::ManagementResponse { .. }
            | Self::ManagementEvent { .. } => BleCommandLane::Control,
        }
    }

    pub const fn class(self) -> CommandClass {
        match self {
            Self::Notify { reason, .. } => match reason {
                NotifyReason::Input => CommandClass::Realtime,
                NotifyReason::InputEdge
                | NotifyReason::InputRelease
                | NotifyReason::TargetSwitchRelease
                | NotifyReason::UsbDeviceRemovedRelease
                | NotifyReason::SafetyRelease => CommandClass::Critical,
            },
            Self::AllowPairing { .. }
            | Self::RejectPairing { .. }
            | Self::ClearBond { .. }
            | Self::ActivateInput { .. }
            | Self::ManagementResponse { .. } => CommandClass::Critical,
            Self::ManagementEvent { .. } => CommandClass::BestEffort,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
// Control requests intentionally remain owned, fixed-capacity values at the
// no_std task boundary. Boxing would add a heap requirement, while a second
// queue would weaken ordering with endpoint output for the same USB device.
#[allow(clippy::large_enum_variant)]
pub enum UsbHostTaskCommand {
    KeyboardLedWrite {
        interface_id: InterfaceId,
        device_id: DeviceId,
        bytes: KeyboardLedOutputBytes,
    },
    #[cfg(feature = "dual-s3-wired")]
    MirrorEndpointOut {
        device_id: DeviceId,
        report: RawEndpointReport,
    },
    #[cfg(feature = "dual-s3-wired")]
    MirrorControlRequest {
        device_id: DeviceId,
        request: MirrorControlRequest,
    },
}

impl UsbHostTaskCommand {
    pub const fn class(self) -> CommandClass {
        match self {
            Self::KeyboardLedWrite { .. } => CommandClass::Realtime,
            #[cfg(feature = "dual-s3-wired")]
            Self::MirrorEndpointOut { .. } | Self::MirrorControlRequest { .. } => {
                CommandClass::Critical
            }
        }
    }

    pub const fn led_target(self) -> Option<(InterfaceId, DeviceId)> {
        match self {
            Self::KeyboardLedWrite {
                interface_id,
                device_id,
                ..
            } => Some((interface_id, device_id)),
            #[cfg(feature = "dual-s3-wired")]
            Self::MirrorEndpointOut { .. } | Self::MirrorControlRequest { .. } => None,
        }
    }

    pub const fn device_id(self) -> DeviceId {
        match self {
            Self::KeyboardLedWrite { device_id, .. } => device_id,
            #[cfg(feature = "dual-s3-wired")]
            Self::MirrorEndpointOut { device_id, .. }
            | Self::MirrorControlRequest { device_id, .. } => device_id,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
// Storage snapshots stay inline so the no_std firmware does not allocate.
#[allow(clippy::large_enum_variant)]
pub enum StorageTaskCommand {
    Persist {
        state: StorageState,
        priority: StoragePersistPriority,
    },
    FactoryReset,
}

impl StorageTaskCommand {
    pub const fn class(&self) -> CommandClass {
        CommandClass::Critical
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusTaskCommand {
    pub status: BridgeStatus,
    pub snapshot: StatusSnapshot,
    pub management: Option<ManagementTaskResponse>,
}

impl StatusTaskCommand {
    pub const fn class(self) -> CommandClass {
        if self.management.is_some() {
            CommandClass::Critical
        } else {
            CommandClass::BestEffort
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementTaskResponse {
    pub destination: ManagementDestination,
    pub response: ManagementResponse,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::management::{ManagementResponsePayload, ManagementResult, ManagementStatus};
    use crate::reports::BleKeyboard6KroReport;

    #[test]
    fn command_classes_match_runtime_delivery_policy() {
        assert!(
            !BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }
            .changes_advertising_policy()
        );
        assert!(BleTaskCommand::AllowPairing { host_id: HostId(1) }.changes_advertising_policy());
        assert!(BleTaskCommand::RejectPairing { host_id: HostId(1) }.changes_advertising_policy());
        assert!(
            BleTaskCommand::ClearBond {
                host_id: HostId(1),
                bond: None,
            }
            .changes_advertising_policy()
        );
        assert!(!BleTaskCommand::ActivateInput { host_id: HostId(1) }.changes_advertising_policy());
        assert_eq!(
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }
            .lane(),
            BleCommandLane::Notify
        );
        assert_eq!(
            BleTaskCommand::AllowPairing { host_id: HostId(1) }.lane(),
            BleCommandLane::Control
        );
        assert_eq!(
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::Input,
            }
            .class(),
            CommandClass::Realtime
        );
        assert_eq!(
            BleTaskCommand::Notify {
                host_id: HostId(1),
                report: BleHidReport::Keyboard(BleKeyboard6KroReport::release()),
                reason: NotifyReason::TargetSwitchRelease,
            }
            .class(),
            CommandClass::Critical
        );
        assert_eq!(
            BleTaskCommand::AllowPairing { host_id: HostId(1) }.class(),
            CommandClass::Critical
        );
        assert_eq!(
            StatusTaskCommand {
                status: BridgeStatus {
                    active_target: None,
                    pairable_host: None,
                },
                snapshot: StatusSnapshot::empty(),
                management: None,
            }
            .class(),
            CommandClass::BestEffort
        );
        assert_eq!(
            StatusTaskCommand {
                status: BridgeStatus {
                    active_target: None,
                    pairable_host: None,
                },
                snapshot: StatusSnapshot::empty(),
                management: Some(ManagementTaskResponse {
                    destination: ManagementDestination::Wired,
                    response: ManagementResponse {
                        request_id: 1,
                        result: ManagementResult::Ok,
                        payload: ManagementResponsePayload::Status(ManagementStatus::empty(4)),
                    },
                }),
            }
            .class(),
            CommandClass::Critical
        );
    }
}
