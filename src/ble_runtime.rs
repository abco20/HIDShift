use crate::ble::BleHidAttribute;
use crate::ids::HostId;
use crate::runtime::message::{
    RuntimeBleGattWrite, RuntimeBleHostEvent, RuntimeInputMessage, RuntimeInputMessageError,
};
use crate::storage::StoredBond;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleHidAttributeHandles {
    pub keyboard_input_cccd: Option<u16>,
    pub mouse_input_cccd: Option<u16>,
    pub consumer_input_cccd: Option<u16>,
    pub keyboard_output_cccd: Option<u16>,
    pub keyboard_output_report: u16,
    pub boot_keyboard_output_report: Option<u16>,
}

impl BleHidAttributeHandles {
    pub fn resolve(self, handle: u16) -> BleHidAttribute {
        if self.keyboard_output_report == handle {
            return BleHidAttribute::KeyboardOutputReport;
        }
        if self.boot_keyboard_output_report == Some(handle) {
            return BleHidAttribute::BootKeyboardOutputReport;
        }
        if self.keyboard_input_cccd == Some(handle) {
            return BleHidAttribute::KeyboardInputCccd;
        }
        if self.mouse_input_cccd == Some(handle) {
            return BleHidAttribute::MouseInputCccd;
        }
        if self.consumer_input_cccd == Some(handle) {
            return BleHidAttribute::ConsumerInputCccd;
        }
        if self.keyboard_output_cccd == Some(handle) {
            return BleHidAttribute::KeyboardOutputCccd;
        }
        BleHidAttribute::Unknown
    }
}

pub fn connected_message(host_id: HostId) -> RuntimeInputMessage {
    RuntimeInputMessage::BleHostEvent {
        host_id,
        event: RuntimeBleHostEvent::Connected,
    }
}

pub fn disconnected_message(host_id: HostId) -> RuntimeInputMessage {
    RuntimeInputMessage::BleHostEvent {
        host_id,
        event: RuntimeBleHostEvent::Disconnected,
    }
}

pub fn security_changed_message(
    host_id: HostId,
    encrypted: bool,
    bonded: bool,
    bond: Option<StoredBond>,
) -> RuntimeInputMessage {
    RuntimeInputMessage::BleHostEvent {
        host_id,
        event: RuntimeBleHostEvent::SecurityChanged {
            encrypted,
            bonded,
            bond,
        },
    }
}

pub fn gatt_write_message(
    host_id: HostId,
    handles: BleHidAttributeHandles,
    handle: u16,
    data: &[u8],
) -> Result<RuntimeInputMessage, RuntimeInputMessageError> {
    Ok(RuntimeInputMessage::BleHostEvent {
        host_id,
        event: RuntimeBleHostEvent::GattWrite {
            attribute: handles.resolve(handle),
            data: RuntimeBleGattWrite::from_slice(data)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDLES: BleHidAttributeHandles = BleHidAttributeHandles {
        keyboard_input_cccd: Some(10),
        mouse_input_cccd: Some(11),
        consumer_input_cccd: Some(12),
        keyboard_output_cccd: Some(13),
        keyboard_output_report: 20,
        boot_keyboard_output_report: Some(21),
    };

    #[test]
    fn handle_table_resolves_known_hid_attributes() {
        assert_eq!(HANDLES.resolve(10), BleHidAttribute::KeyboardInputCccd);
        assert_eq!(HANDLES.resolve(11), BleHidAttribute::MouseInputCccd);
        assert_eq!(HANDLES.resolve(12), BleHidAttribute::ConsumerInputCccd);
        assert_eq!(HANDLES.resolve(13), BleHidAttribute::KeyboardOutputCccd);
        assert_eq!(HANDLES.resolve(20), BleHidAttribute::KeyboardOutputReport);
        assert_eq!(
            HANDLES.resolve(21),
            BleHidAttribute::BootKeyboardOutputReport
        );
        assert_eq!(HANDLES.resolve(99), BleHidAttribute::Unknown);
    }

    #[test]
    fn connection_state_messages_become_owned_runtime_inputs() {
        assert_eq!(
            connected_message(HostId(1)),
            RuntimeInputMessage::BleHostEvent {
                host_id: HostId(1),
                event: RuntimeBleHostEvent::Connected,
            }
        );
        assert_eq!(
            disconnected_message(HostId(2)),
            RuntimeInputMessage::BleHostEvent {
                host_id: HostId(2),
                event: RuntimeBleHostEvent::Disconnected,
            }
        );
        assert_eq!(
            security_changed_message(HostId(3), true, false, None),
            RuntimeInputMessage::BleHostEvent {
                host_id: HostId(3),
                event: RuntimeBleHostEvent::SecurityChanged {
                    encrypted: true,
                    bonded: false,
                    bond: None,
                },
            }
        );
    }

    #[test]
    fn gatt_write_message_resolves_attribute_and_owns_payload() {
        assert_eq!(
            gatt_write_message(HostId(1), HANDLES, 20, &[1, 2]).unwrap(),
            RuntimeInputMessage::BleHostEvent {
                host_id: HostId(1),
                event: RuntimeBleHostEvent::GattWrite {
                    attribute: BleHidAttribute::KeyboardOutputReport,
                    data: RuntimeBleGattWrite::from_slice(&[1, 2]).unwrap(),
                },
            }
        );
    }

    #[test]
    fn gatt_write_message_rejects_payloads_beyond_runtime_boundary() {
        assert_eq!(
            gatt_write_message(HostId(1), HANDLES, 20, &[0, 1, 2]),
            Err(RuntimeInputMessageError::BleGattWriteTooLong)
        );
    }
}
