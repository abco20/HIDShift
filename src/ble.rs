use crate::bridge::{BridgeEvent, keyboard_led_event_from_ble_output};
use crate::ids::HostId;
use crate::reports::{BleKeyboardOutputError, KEYBOARD_REPORT_ID, ReportKind};
use crate::storage::StoredBond;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleHidAttribute {
    KeyboardInputCccd,
    MouseInputCccd,
    ConsumerInputCccd,
    KeyboardOutputCccd,
    KeyboardOutputReport,
    BootKeyboardOutputReport,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleHostAdapterEvent<'a> {
    Connected,
    Disconnected,
    SecurityChanged {
        encrypted: bool,
        bonded: bool,
        bond: Option<StoredBond>,
    },
    GattWrite {
        attribute: BleHidAttribute,
        data: &'a [u8],
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleHostAdapterError {
    Output(BleKeyboardOutputError),
    InvalidCccdLength,
    InvalidOutputReportLength,
    EventCapacity,
}

impl From<BleKeyboardOutputError> for BleHostAdapterError {
    fn from(error: BleKeyboardOutputError) -> Self {
        Self::Output(error)
    }
}

pub fn bridge_events_from_ble_host_event<const N: usize>(
    host_id: HostId,
    event: BleHostAdapterEvent<'_>,
    out: &mut heapless::Vec<BridgeEvent, N>,
) -> Result<(), BleHostAdapterError> {
    out.clear();

    match event {
        BleHostAdapterEvent::Connected => push_event(out, BridgeEvent::HostConnected { host_id }),
        BleHostAdapterEvent::Disconnected => {
            push_event(out, BridgeEvent::HostDisconnected { host_id })
        }
        BleHostAdapterEvent::SecurityChanged {
            encrypted,
            bonded,
            bond,
        } => push_event(
            out,
            BridgeEvent::HostSecurityChanged {
                host_id,
                encrypted,
                bonded,
                bond,
            },
        ),
        BleHostAdapterEvent::GattWrite { attribute, data } => {
            bridge_events_from_gatt_write(host_id, attribute, data, out)
        }
    }
}

fn bridge_events_from_gatt_write<const N: usize>(
    host_id: HostId,
    attribute: BleHidAttribute,
    data: &[u8],
    out: &mut heapless::Vec<BridgeEvent, N>,
) -> Result<(), BleHostAdapterError> {
    match attribute {
        BleHidAttribute::KeyboardInputCccd => {
            push_cccd_event(host_id, ReportKind::Keyboard, data, out)
        }
        BleHidAttribute::MouseInputCccd => push_cccd_event(host_id, ReportKind::Mouse, data, out),
        BleHidAttribute::ConsumerInputCccd => {
            push_cccd_event(host_id, ReportKind::Consumer, data, out)
        }
        BleHidAttribute::KeyboardOutputCccd => {
            push_cccd_event(host_id, ReportKind::KeyboardOutput, data, out)
        }
        BleHidAttribute::KeyboardOutputReport => {
            let payload = keyboard_output_report_payload(data)?;
            push_event(out, keyboard_led_event_from_ble_output(host_id, payload)?)
        }
        BleHidAttribute::BootKeyboardOutputReport => {
            push_event(out, keyboard_led_event_from_ble_output(host_id, data)?)
        }
        BleHidAttribute::Unknown => Ok(()),
    }
}

fn push_cccd_event<const N: usize>(
    host_id: HostId,
    report: ReportKind,
    data: &[u8],
    out: &mut heapless::Vec<BridgeEvent, N>,
) -> Result<(), BleHostAdapterError> {
    push_event(
        out,
        BridgeEvent::CccdChanged {
            host_id,
            report,
            enabled: cccd_notify_enabled(data)?,
        },
    )
}

pub fn cccd_notify_enabled(data: &[u8]) -> Result<bool, BleHostAdapterError> {
    let [lo, hi] = data else {
        return Err(BleHostAdapterError::InvalidCccdLength);
    };

    Ok(u16::from_le_bytes([*lo, *hi]) & 0x0001 != 0)
}

fn keyboard_output_report_payload(data: &[u8]) -> Result<&[u8], BleHostAdapterError> {
    match data {
        [bits] => Ok(core::slice::from_ref(bits)),
        [report_id, bits] if *report_id == KEYBOARD_REPORT_ID => Ok(core::slice::from_ref(bits)),
        _ => Err(BleHostAdapterError::InvalidOutputReportLength),
    }
}

fn push_event<const N: usize>(
    out: &mut heapless::Vec<BridgeEvent, N>,
    event: BridgeEvent,
) -> Result<(), BleHostAdapterError> {
    out.push(event)
        .map_err(|_| BleHostAdapterError::EventCapacity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyboardLedState;

    const HOST: HostId = HostId(1);

    #[test]
    fn connection_and_security_events_map_to_bridge_events() {
        let mut events = heapless::Vec::<BridgeEvent, 2>::new();

        bridge_events_from_ble_host_event(HOST, BleHostAdapterEvent::Connected, &mut events)
            .unwrap();
        assert_eq!(
            events.as_slice(),
            &[BridgeEvent::HostConnected { host_id: HOST }]
        );

        bridge_events_from_ble_host_event(
            HOST,
            BleHostAdapterEvent::SecurityChanged {
                encrypted: true,
                bonded: true,
                bond: None,
            },
            &mut events,
        )
        .unwrap();
        assert_eq!(
            events.as_slice(),
            &[BridgeEvent::HostSecurityChanged {
                host_id: HOST,
                encrypted: true,
                bonded: true,
                bond: None,
            }]
        );
    }

    #[test]
    fn cccd_writes_map_to_report_readiness_events() {
        let mut events = heapless::Vec::<BridgeEvent, 1>::new();

        bridge_events_from_ble_host_event(
            HOST,
            BleHostAdapterEvent::GattWrite {
                attribute: BleHidAttribute::MouseInputCccd,
                data: &[0x01, 0x00],
            },
            &mut events,
        )
        .unwrap();

        assert_eq!(
            events.as_slice(),
            &[BridgeEvent::CccdChanged {
                host_id: HOST,
                report: ReportKind::Mouse,
                enabled: true,
            }]
        );
    }

    #[test]
    fn keyboard_output_report_write_maps_to_led_event_with_or_without_report_id() {
        let mut events = heapless::Vec::<BridgeEvent, 1>::new();

        bridge_events_from_ble_host_event(
            HOST,
            BleHostAdapterEvent::GattWrite {
                attribute: BleHidAttribute::KeyboardOutputReport,
                data: &[KEYBOARD_REPORT_ID, 0b0000_0010],
            },
            &mut events,
        )
        .unwrap();
        assert_eq!(
            events.as_slice(),
            &[BridgeEvent::HostKeyboardLedChanged {
                host_id: HOST,
                leds: KeyboardLedState::CAPS_LOCK,
            }]
        );

        bridge_events_from_ble_host_event(
            HOST,
            BleHostAdapterEvent::GattWrite {
                attribute: BleHidAttribute::BootKeyboardOutputReport,
                data: &[0b0000_0101],
            },
            &mut events,
        )
        .unwrap();
        assert_eq!(
            events.as_slice(),
            &[BridgeEvent::HostKeyboardLedChanged {
                host_id: HOST,
                leds: KeyboardLedState::NUM_LOCK | KeyboardLedState::SCROLL_LOCK,
            }]
        );
    }

    #[test]
    fn invalid_cccd_and_output_writes_are_explicit_errors() {
        let mut events = heapless::Vec::<BridgeEvent, 1>::new();

        assert_eq!(
            bridge_events_from_ble_host_event(
                HOST,
                BleHostAdapterEvent::GattWrite {
                    attribute: BleHidAttribute::KeyboardInputCccd,
                    data: &[0x01],
                },
                &mut events,
            ),
            Err(BleHostAdapterError::InvalidCccdLength)
        );
        assert_eq!(
            bridge_events_from_ble_host_event(
                HOST,
                BleHostAdapterEvent::GattWrite {
                    attribute: BleHidAttribute::KeyboardOutputReport,
                    data: &[0xff, 0x00],
                },
                &mut events,
            ),
            Err(BleHostAdapterError::InvalidOutputReportLength)
        );
    }
}
