use crate::ble::{BleHidAttribute, BleHostAdapterEvent};
use crate::bridge::BridgeEvent;
use crate::ids::{DeviceId, HostId, InterfaceId};
use crate::management::{ManagementDestination, ManagementRequest};
use crate::storage::{StorageState, StoredBond};
use crate::target_control::ButtonIntent;
use crate::usb_hid::output::KeyboardLedOutputReport;

use super::{RUNTIME_BLE_GATT_WRITE_MAX_LEN, RuntimeDiagnosticsEvent, RuntimeInput};

#[derive(Clone, Debug, Eq, PartialEq)]
// Fixed-capacity no_std messages deliberately carry storage snapshots inline.
#[allow(clippy::large_enum_variant)]
pub enum RuntimeInputMessage {
    BridgeEvent(BridgeEvent),
    ButtonIntent {
        intent: ButtonIntent,
        now_ms: u64,
    },
    ManagementRequest {
        destination: ManagementDestination,
        request: ManagementRequest,
        now_ms: u64,
    },
    Tick {
        now_ms: u64,
    },
    BleHostEvent {
        host_id: HostId,
        event: RuntimeBleHostEvent,
    },
    UsbHidInterfaceConnected {
        interface_id: InterfaceId,
        device_id: DeviceId,
        led_output: Option<KeyboardLedOutputReport>,
    },
    UsbHidInterfaceDisconnected {
        interface_id: InterfaceId,
    },
    UsbDeviceMetadataUpdated {
        device_id: DeviceId,
        vendor_id: u16,
        product_id: u16,
        name: crate::storage::FixedName,
        flags: u8,
    },
    HostNameDiscovered {
        host_id: HostId,
        name: crate::storage::FixedName,
    },
    DiagnosticsEvent(RuntimeDiagnosticsEvent),
    #[cfg(feature = "dual-s3-wired")]
    DeviceProfileResult(crate::interchip::ProfileResult),
    #[cfg(feature = "dual-s3-wired")]
    MirrorEndpointOut(crate::interchip::RawEndpointReport),
    #[cfg(feature = "dual-s3-wired")]
    MirrorControlRequest(crate::interchip::MirrorControlRequest),
    #[cfg(feature = "dual-s3-wired")]
    MirrorCandidateRegistered {
        candidate: crate::output_target::MirrorCandidateId,
        profile_hash: Option<u32>,
    },
    RestoreStorage(StorageState),
}

impl RuntimeInputMessage {
    pub fn as_runtime_input(&self) -> RuntimeInput<'_> {
        match self {
            Self::BridgeEvent(event) => RuntimeInput::BridgeEvent(event.clone()),
            Self::ButtonIntent { intent, now_ms } => RuntimeInput::ButtonIntent {
                intent: *intent,
                now_ms: *now_ms,
            },
            Self::ManagementRequest {
                destination,
                request,
                now_ms,
            } => RuntimeInput::ManagementRequest {
                destination: *destination,
                request: *request,
                now_ms: *now_ms,
            },
            Self::Tick { now_ms } => RuntimeInput::Tick { now_ms: *now_ms },
            Self::BleHostEvent { host_id, event } => RuntimeInput::BleHostEvent {
                host_id: *host_id,
                event: event.as_borrowed(),
            },
            Self::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                led_output,
            } => RuntimeInput::UsbHidInterfaceConnected {
                interface_id: *interface_id,
                device_id: *device_id,
                led_output: *led_output,
            },
            Self::UsbHidInterfaceDisconnected { interface_id } => {
                RuntimeInput::UsbHidInterfaceDisconnected {
                    interface_id: *interface_id,
                }
            }
            Self::UsbDeviceMetadataUpdated {
                device_id,
                vendor_id,
                product_id,
                name,
                flags,
            } => RuntimeInput::UsbDeviceMetadataUpdated {
                device_id: *device_id,
                vendor_id: *vendor_id,
                product_id: *product_id,
                name: *name,
                flags: *flags,
            },
            Self::HostNameDiscovered { host_id, name } => RuntimeInput::HostNameDiscovered {
                host_id: *host_id,
                name: *name,
            },
            Self::DiagnosticsEvent(event) => RuntimeInput::DiagnosticsEvent(*event),
            #[cfg(feature = "dual-s3-wired")]
            Self::DeviceProfileResult(result) => RuntimeInput::DeviceProfileResult(*result),
            #[cfg(feature = "dual-s3-wired")]
            Self::MirrorEndpointOut(report) => RuntimeInput::MirrorEndpointOut(*report),
            #[cfg(feature = "dual-s3-wired")]
            Self::MirrorControlRequest(request) => RuntimeInput::MirrorControlRequest(*request),
            #[cfg(feature = "dual-s3-wired")]
            Self::MirrorCandidateRegistered {
                candidate,
                profile_hash,
            } => RuntimeInput::MirrorCandidateRegistered {
                candidate: *candidate,
                profile_hash: *profile_hash,
            },
            Self::RestoreStorage(storage) => RuntimeInput::RestoreStorage(storage),
        }
    }
}

impl TryFrom<RuntimeInput<'_>> for RuntimeInputMessage {
    type Error = RuntimeInputMessageError;

    fn try_from(input: RuntimeInput<'_>) -> Result<Self, Self::Error> {
        match input {
            RuntimeInput::BridgeEvent(event) => Ok(Self::BridgeEvent(event)),
            RuntimeInput::ButtonIntent { intent, now_ms } => {
                Ok(Self::ButtonIntent { intent, now_ms })
            }
            RuntimeInput::ManagementRequest {
                destination,
                request,
                now_ms,
            } => Ok(Self::ManagementRequest {
                destination,
                request,
                now_ms,
            }),
            RuntimeInput::Tick { now_ms } => Ok(Self::Tick { now_ms }),
            RuntimeInput::BleHostEvent { host_id, event } => Ok(Self::BleHostEvent {
                host_id,
                event: RuntimeBleHostEvent::try_from(event)?,
            }),
            RuntimeInput::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                led_output,
            } => Ok(Self::UsbHidInterfaceConnected {
                interface_id,
                device_id,
                led_output,
            }),
            RuntimeInput::UsbHidInterfaceDisconnected { interface_id } => {
                Ok(Self::UsbHidInterfaceDisconnected { interface_id })
            }
            RuntimeInput::UsbDeviceMetadataUpdated {
                device_id,
                vendor_id,
                product_id,
                name,
                flags,
            } => Ok(Self::UsbDeviceMetadataUpdated {
                device_id,
                vendor_id,
                product_id,
                name,
                flags,
            }),
            RuntimeInput::HostNameDiscovered { host_id, name } => {
                Ok(Self::HostNameDiscovered { host_id, name })
            }
            RuntimeInput::DiagnosticsEvent(event) => Ok(Self::DiagnosticsEvent(event)),
            #[cfg(feature = "dual-s3-wired")]
            RuntimeInput::DeviceProfileResult(result) => Ok(Self::DeviceProfileResult(result)),
            #[cfg(feature = "dual-s3-wired")]
            RuntimeInput::MirrorEndpointOut(report) => Ok(Self::MirrorEndpointOut(report)),
            #[cfg(feature = "dual-s3-wired")]
            RuntimeInput::MirrorControlRequest(request) => Ok(Self::MirrorControlRequest(request)),
            #[cfg(feature = "dual-s3-wired")]
            RuntimeInput::MirrorCandidateRegistered {
                candidate,
                profile_hash,
            } => Ok(Self::MirrorCandidateRegistered {
                candidate,
                profile_hash,
            }),
            RuntimeInput::RestoreStorage(storage) => Ok(Self::RestoreStorage(storage.clone())),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeBleHostEvent {
    Connected,
    Disconnected,
    SecurityChanged {
        encrypted: bool,
        bonded: bool,
        bond: Option<StoredBond>,
    },
    GattWrite {
        attribute: BleHidAttribute,
        data: RuntimeBleGattWrite,
    },
}

impl RuntimeBleHostEvent {
    pub fn as_borrowed(&self) -> BleHostAdapterEvent<'_> {
        match self {
            Self::Connected => BleHostAdapterEvent::Connected,
            Self::Disconnected => BleHostAdapterEvent::Disconnected,
            Self::SecurityChanged {
                encrypted,
                bonded,
                bond,
            } => BleHostAdapterEvent::SecurityChanged {
                encrypted: *encrypted,
                bonded: *bonded,
                bond: *bond,
            },
            Self::GattWrite { attribute, data } => BleHostAdapterEvent::GattWrite {
                attribute: *attribute,
                data: data.as_slice(),
            },
        }
    }
}

impl TryFrom<BleHostAdapterEvent<'_>> for RuntimeBleHostEvent {
    type Error = RuntimeInputMessageError;

    fn try_from(event: BleHostAdapterEvent<'_>) -> Result<Self, Self::Error> {
        match event {
            BleHostAdapterEvent::Connected => Ok(Self::Connected),
            BleHostAdapterEvent::Disconnected => Ok(Self::Disconnected),
            BleHostAdapterEvent::SecurityChanged {
                encrypted,
                bonded,
                bond,
            } => Ok(Self::SecurityChanged {
                encrypted,
                bonded,
                bond,
            }),
            BleHostAdapterEvent::GattWrite { attribute, data } => Ok(Self::GattWrite {
                attribute,
                data: RuntimeBleGattWrite::from_slice(data)?,
            }),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeBleGattWrite {
    len: u8,
    bytes: [u8; RUNTIME_BLE_GATT_WRITE_MAX_LEN],
}

impl RuntimeBleGattWrite {
    pub fn from_slice(data: &[u8]) -> Result<Self, RuntimeInputMessageError> {
        if data.len() > RUNTIME_BLE_GATT_WRITE_MAX_LEN {
            return Err(RuntimeInputMessageError::BleGattWriteTooLong);
        }

        let mut bytes = [0u8; RUNTIME_BLE_GATT_WRITE_MAX_LEN];
        bytes[..data.len()].copy_from_slice(data);
        Ok(Self {
            len: data.len() as u8,
            bytes,
        })
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeInputMessageError {
    BleGattWriteTooLong,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ble::BleHidAttribute;
    use crate::ids::{InterfaceId, ReportId};
    use crate::input::{HidDirection, InputFrame, KeyboardLedState, VendorHidFrame};

    #[test]
    fn borrowed_ble_gatt_write_becomes_owned_runtime_message() {
        let message = RuntimeInputMessage::try_from(RuntimeInput::BleHostEvent {
            host_id: HostId(2),
            event: BleHostAdapterEvent::GattWrite {
                attribute: BleHidAttribute::KeyboardOutputReport,
                data: &[1, 2],
            },
        })
        .unwrap();

        assert_eq!(
            message,
            RuntimeInputMessage::BleHostEvent {
                host_id: HostId(2),
                event: RuntimeBleHostEvent::GattWrite {
                    attribute: BleHidAttribute::KeyboardOutputReport,
                    data: RuntimeBleGattWrite::from_slice(&[1, 2]).unwrap(),
                },
            }
        );
    }

    #[test]
    fn owned_runtime_message_round_trips_back_to_borrowed_runtime_input() {
        let mut storage = StorageState::new(12);
        storage.last_active_host = Some(HostId(1));
        let message = RuntimeInputMessage::RestoreStorage(storage.clone());

        let RuntimeInput::RestoreStorage(restored) = message.as_runtime_input() else {
            panic!("expected storage restore input");
        };

        assert_eq!(restored, &storage);
    }

    #[test]
    fn owned_gatt_write_rejects_payloads_longer_than_runtime_boundary() {
        assert_eq!(
            RuntimeBleGattWrite::from_slice(&[0, 1, 2]),
            Err(RuntimeInputMessageError::BleGattWriteTooLong)
        );
    }

    #[test]
    fn bridge_event_message_keeps_full_owned_event() {
        let vendor = VendorHidFrame::new(
            DeviceId(9),
            InterfaceId(3),
            0xff00,
            Some(ReportId(1)),
            HidDirection::Input,
            &[0xaa, 0xbb],
        )
        .unwrap();
        let message = RuntimeInputMessage::try_from(RuntimeInput::BridgeEvent(
            BridgeEvent::InputFrame(InputFrame::Vendor(vendor)),
        ))
        .unwrap();

        let RuntimeInputMessage::BridgeEvent(BridgeEvent::InputFrame(InputFrame::Vendor(frame))) =
            message
        else {
            panic!("expected owned bridge event");
        };

        assert_eq!(frame.device_id, DeviceId(9));
        assert_eq!(frame.interface_id, InterfaceId(3));
        assert_eq!(frame.usage_page, 0xff00);
        assert_eq!(frame.report_id, Some(ReportId(1)));
        assert_eq!(frame.direction, HidDirection::Input);
        assert_eq!(frame.payload(), &[0xaa, 0xbb]);
    }

    #[cfg(feature = "dual-s3-wired")]
    #[test]
    fn profile_result_message_reaches_runtime_without_transport_borrowing() {
        let result = crate::interchip::ProfileResult {
            transfer_id: 7,
            profile_hash: 9,
            status: crate::interchip::ProfileResultStatus::Accepted,
            reject_reason: 0,
            detail: 0,
        };
        let message = RuntimeInputMessage::DeviceProfileResult(result);
        assert_eq!(
            message.as_runtime_input(),
            RuntimeInput::DeviceProfileResult(result)
        );
    }

    #[test]
    fn owned_ble_event_borrows_payload_without_reallocation() {
        let message = RuntimeInputMessage::BleHostEvent {
            host_id: HostId(1),
            event: RuntimeBleHostEvent::GattWrite {
                attribute: BleHidAttribute::BootKeyboardOutputReport,
                data: RuntimeBleGattWrite::from_slice(&[KeyboardLedState::CAPS_LOCK.bits()])
                    .unwrap(),
            },
        };

        let RuntimeInput::BleHostEvent { host_id, event } = message.as_runtime_input() else {
            panic!("expected ble host input");
        };
        let BleHostAdapterEvent::GattWrite { attribute, data } = event else {
            panic!("expected gatt write event");
        };

        assert_eq!(host_id, HostId(1));
        assert_eq!(attribute, BleHidAttribute::BootKeyboardOutputReport);
        assert_eq!(data, &[KeyboardLedState::CAPS_LOCK.bits()]);
    }
}
