//! Fixed-size, allocation-free protocol used by the hardware E2E harness.
//!
//! This protocol deliberately enters at the normalized input-frame boundary. It
//! does not pretend to test USB enumeration or HID descriptor parsing.

use crate::ids::{DeviceId, InterfaceId};
use crate::input::{
    ConsumerFrame, ConsumerUsage, InputFrame, KeyUsage, KeyboardFrame, ModifierState, MouseButtons,
    MouseFrame, MouseMovement, StandardInputFrame,
};
use crate::transport::InputTransport;

pub const E2E_PROTOCOL_VERSION: u8 = 1;
pub const E2E_PACKET_LEN: usize = 20;
pub const E2E_LINE_PREFIX: &[u8] = b"@HIDSHIFT-E2E:";
pub const E2E_LINE_LEN: usize = E2E_LINE_PREFIX.len() + E2E_PACKET_LEN * 2;
/// Raw little-endian HCI address used by the dedicated test probe.
pub const E2E_PROBE_BLE_ADDRESS_RAW: [u8; 6] = [0x01, 0xe2, 0xe2, 0xe2, 0xe2, 0xc2];

const OP_HELLO: u8 = 0x01;
const OP_RELEASE_ALL: u8 = 0x02;
const OP_READ_TIMESTAMP: u8 = 0x03;
const OP_ENTER_DEVICE_DOWNLOAD: u8 = 0x04;
const OP_DROP_NEXT_INPUT: u8 = 0x05;
const OP_DROP_NEXT_INPUT_BURST: u8 = 0x06;
const OP_SELECT_TRANSPORT: u8 = 0x07;
const OP_KEYBOARD: u8 = 0x10;
const OP_MOUSE: u8 = 0x11;
const OP_CONSUMER: u8 = 0x12;
const OP_VENDOR_INPUT: u8 = 0x13;
const OP_MOUSE_BURST: u8 = 0x14;

const TEST_DEVICE_ID: DeviceId = DeviceId(0xfe);
const TEST_INTERFACE_ID: InterfaceId = InterfaceId(0xfe);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct E2ePacket {
    pub sequence: u32,
    pub command: E2eCommand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum E2eCommand {
    Hello,
    ReadTimestamp {
        target_sequence: u32,
    },
    EnterDeviceDownload,
    DropNextInput {
        lane: E2eInputLane,
    },
    /// Drops the primary frame and suppresses its timed recovery snapshots.
    /// The following new input must recover it from the rolling journal.
    DropNextInputBurst {
        lane: E2eInputLane,
    },
    SelectTransport {
        transport: InputTransport,
    },
    ReleaseAll,
    Keyboard {
        modifiers: u8,
        keys: [u8; 6],
    },
    Mouse {
        buttons: u8,
        x: i16,
        y: i16,
        wheel: i8,
        pan: i8,
    },
    /// Generates several motion reports inside firmware from one UART packet.
    /// This isolates radio-scheduler pressure from diagnostic UART throughput.
    MouseBurst {
        count: u8,
        x: i16,
        y: i16,
    },
    Consumer {
        usage: u16,
    },
    /// Generates a deterministic vendor report in bridge firmware. The data
    /// itself is expanded there so a 63-byte report fits this fixed UART packet.
    VendorInput {
        len: u8,
        seed: u8,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum E2eInputLane {
    Motion = 1,
    Critical = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum E2eProtocolError {
    InvalidLength,
    InvalidPrefix,
    InvalidHex,
    UnsupportedVersion,
    UnknownCommand,
    InvalidPayload,
    InvalidChecksum,
}

impl E2ePacket {
    /// Only handshake packets need a UART acknowledgement from the DUT.
    ///
    /// Input packets deliberately avoid synchronous logging because UART log
    /// output is slow enough to perturb the latency being measured.
    pub const fn requests_acknowledgement(self) -> bool {
        matches!(
            self.command,
            E2eCommand::Hello
                | E2eCommand::EnterDeviceDownload
                | E2eCommand::SelectTransport { .. }
        )
    }

    pub const fn carries_input(self) -> bool {
        matches!(
            self.command,
            E2eCommand::ReleaseAll
                | E2eCommand::DropNextInput { .. }
                | E2eCommand::DropNextInputBurst { .. }
                | E2eCommand::Keyboard { .. }
                | E2eCommand::Mouse { .. }
                | E2eCommand::MouseBurst { .. }
                | E2eCommand::Consumer { .. }
                | E2eCommand::VendorInput { .. }
        )
    }

    pub fn encode(self) -> [u8; E2E_PACKET_LEN] {
        let mut bytes = [0; E2E_PACKET_LEN];
        bytes[0] = E2E_PROTOCOL_VERSION;
        bytes[2..6].copy_from_slice(&self.sequence.to_le_bytes());
        match self.command {
            E2eCommand::Hello => bytes[1] = OP_HELLO,
            E2eCommand::ReadTimestamp { target_sequence } => {
                bytes[1] = OP_READ_TIMESTAMP;
                bytes[6..10].copy_from_slice(&target_sequence.to_le_bytes());
            }
            E2eCommand::EnterDeviceDownload => bytes[1] = OP_ENTER_DEVICE_DOWNLOAD,
            E2eCommand::DropNextInput { lane } => {
                bytes[1] = OP_DROP_NEXT_INPUT;
                bytes[6] = lane as u8;
            }
            E2eCommand::DropNextInputBurst { lane } => {
                bytes[1] = OP_DROP_NEXT_INPUT_BURST;
                bytes[6] = lane as u8;
            }
            E2eCommand::SelectTransport { transport } => {
                bytes[1] = OP_SELECT_TRANSPORT;
                bytes[6] = transport as u8;
            }
            E2eCommand::ReleaseAll => bytes[1] = OP_RELEASE_ALL,
            E2eCommand::Keyboard { modifiers, keys } => {
                bytes[1] = OP_KEYBOARD;
                bytes[6] = modifiers;
                bytes[7..13].copy_from_slice(&keys);
            }
            E2eCommand::Mouse {
                buttons,
                x,
                y,
                wheel,
                pan,
            } => {
                bytes[1] = OP_MOUSE;
                bytes[6] = buttons;
                bytes[7..9].copy_from_slice(&x.to_le_bytes());
                bytes[9..11].copy_from_slice(&y.to_le_bytes());
                bytes[11] = wheel as u8;
                bytes[12] = pan as u8;
            }
            E2eCommand::Consumer { usage } => {
                bytes[1] = OP_CONSUMER;
                bytes[6..8].copy_from_slice(&usage.to_le_bytes());
            }
            E2eCommand::VendorInput { len, seed } => {
                bytes[1] = OP_VENDOR_INPUT;
                bytes[6] = len;
                bytes[7] = seed;
            }
            E2eCommand::MouseBurst { count, x, y } => {
                bytes[1] = OP_MOUSE_BURST;
                bytes[6] = count;
                bytes[7..9].copy_from_slice(&x.to_le_bytes());
                bytes[9..11].copy_from_slice(&y.to_le_bytes());
            }
        }
        let checksum = crc16_ccitt_false(&bytes[..E2E_PACKET_LEN - 2]);
        bytes[E2E_PACKET_LEN - 2..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, E2eProtocolError> {
        if bytes.len() != E2E_PACKET_LEN {
            return Err(E2eProtocolError::InvalidLength);
        }
        let expected = u16::from_le_bytes([bytes[E2E_PACKET_LEN - 2], bytes[E2E_PACKET_LEN - 1]]);
        if crc16_ccitt_false(&bytes[..E2E_PACKET_LEN - 2]) != expected {
            return Err(E2eProtocolError::InvalidChecksum);
        }
        if bytes[0] != E2E_PROTOCOL_VERSION {
            return Err(E2eProtocolError::UnsupportedVersion);
        }
        let sequence = u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]);
        let command = match bytes[1] {
            OP_HELLO => E2eCommand::Hello,
            OP_READ_TIMESTAMP => E2eCommand::ReadTimestamp {
                target_sequence: u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]),
            },
            OP_ENTER_DEVICE_DOWNLOAD => E2eCommand::EnterDeviceDownload,
            OP_DROP_NEXT_INPUT => E2eCommand::DropNextInput {
                lane: match bytes[6] {
                    1 => E2eInputLane::Motion,
                    2 => E2eInputLane::Critical,
                    _ => return Err(E2eProtocolError::InvalidPayload),
                },
            },
            OP_DROP_NEXT_INPUT_BURST => E2eCommand::DropNextInputBurst {
                lane: match bytes[6] {
                    1 => E2eInputLane::Motion,
                    2 => E2eInputLane::Critical,
                    _ => return Err(E2eProtocolError::InvalidPayload),
                },
            },
            OP_SELECT_TRANSPORT => E2eCommand::SelectTransport {
                transport: match bytes[6] {
                    1 => InputTransport::Ble,
                    2 => InputTransport::EspNow,
                    _ => return Err(E2eProtocolError::InvalidPayload),
                },
            },
            OP_RELEASE_ALL => E2eCommand::ReleaseAll,
            OP_KEYBOARD => E2eCommand::Keyboard {
                modifiers: bytes[6],
                keys: [
                    bytes[7], bytes[8], bytes[9], bytes[10], bytes[11], bytes[12],
                ],
            },
            OP_MOUSE => E2eCommand::Mouse {
                buttons: bytes[6],
                x: i16::from_le_bytes([bytes[7], bytes[8]]),
                y: i16::from_le_bytes([bytes[9], bytes[10]]),
                wheel: bytes[11] as i8,
                pan: bytes[12] as i8,
            },
            OP_CONSUMER => E2eCommand::Consumer {
                usage: u16::from_le_bytes([bytes[6], bytes[7]]),
            },
            OP_VENDOR_INPUT => E2eCommand::VendorInput {
                len: bytes[6],
                seed: bytes[7],
            },
            OP_MOUSE_BURST if bytes[6] != 0 => E2eCommand::MouseBurst {
                count: bytes[6],
                x: i16::from_le_bytes([bytes[7], bytes[8]]),
                y: i16::from_le_bytes([bytes[9], bytes[10]]),
            },
            OP_MOUSE_BURST => return Err(E2eProtocolError::InvalidPayload),
            _ => return Err(E2eProtocolError::UnknownCommand),
        };
        Ok(Self { sequence, command })
    }

    pub fn decode_line(line: &[u8]) -> Result<Self, E2eProtocolError> {
        if line.len() != E2E_LINE_LEN {
            return Err(E2eProtocolError::InvalidLength);
        }
        if !line.starts_with(E2E_LINE_PREFIX) {
            return Err(E2eProtocolError::InvalidPrefix);
        }
        let encoded = &line[E2E_LINE_PREFIX.len()..];
        let mut bytes = [0; E2E_PACKET_LEN];
        for (index, output) in bytes.iter_mut().enumerate() {
            let high = hex_nibble(encoded[index * 2]).ok_or(E2eProtocolError::InvalidHex)?;
            let low = hex_nibble(encoded[index * 2 + 1]).ok_or(E2eProtocolError::InvalidHex)?;
            *output = (high << 4) | low;
        }
        Self::decode(&bytes)
    }

    pub fn encode_line(self) -> [u8; E2E_LINE_LEN] {
        let mut line = [0; E2E_LINE_LEN];
        line[..E2E_LINE_PREFIX.len()].copy_from_slice(E2E_LINE_PREFIX);
        for (index, byte) in self.encode().iter().copied().enumerate() {
            line[E2E_LINE_PREFIX.len() + index * 2] = hex_digit(byte >> 4);
            line[E2E_LINE_PREFIX.len() + index * 2 + 1] = hex_digit(byte & 0x0f);
        }
        line
    }

    pub fn input_frames(self) -> Result<[Option<InputFrame>; 3], E2eProtocolError> {
        let standard = |keyboard, mouse, consumer| {
            InputFrame::Standard(StandardInputFrame {
                device_id: TEST_DEVICE_ID,
                interface_id: TEST_INTERFACE_ID,
                keyboard,
                mouse,
                consumer,
            })
        };
        match self.command {
            E2eCommand::Hello
            | E2eCommand::ReadTimestamp { .. }
            | E2eCommand::EnterDeviceDownload
            | E2eCommand::DropNextInput { .. }
            | E2eCommand::DropNextInputBurst { .. }
            | E2eCommand::MouseBurst { .. }
            | E2eCommand::SelectTransport { .. } => Ok([None, None, None]),
            E2eCommand::VendorInput { .. } => Ok([None, None, None]),
            E2eCommand::ReleaseAll => Ok([
                Some(standard(
                    Some(KeyboardFrame::new(ModifierState::empty())),
                    None,
                    None,
                )),
                Some(standard(
                    None,
                    Some(MouseFrame {
                        buttons: MouseButtons::empty(),
                        movement: MouseMovement::neutral(),
                    }),
                    None,
                )),
                Some(standard(None, None, Some(ConsumerFrame { active: None }))),
            ]),
            E2eCommand::Keyboard { modifiers, keys } => {
                let mut frame = KeyboardFrame::new(ModifierState::from_bits_truncate(modifiers));
                for key in keys.into_iter().filter(|key| *key != 0) {
                    frame
                        .push_key(KeyUsage(key))
                        .map_err(|_| E2eProtocolError::InvalidPayload)?;
                }
                Ok([Some(standard(Some(frame), None, None)), None, None])
            }
            E2eCommand::Mouse {
                buttons,
                x,
                y,
                wheel,
                pan,
            } => Ok([
                Some(standard(
                    None,
                    Some(MouseFrame {
                        buttons: MouseButtons::from_bits_truncate(buttons),
                        movement: MouseMovement { x, y, wheel, pan },
                    }),
                    None,
                )),
                None,
                None,
            ]),
            E2eCommand::Consumer { usage } => Ok([
                Some(standard(
                    None,
                    None,
                    Some(ConsumerFrame {
                        active: (usage != 0).then_some(ConsumerUsage(usage)),
                    }),
                )),
                None,
                None,
            ]),
        }
    }
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

const fn hex_digit(value: u8) -> u8 {
    if value < 10 {
        b'0' + value
    } else {
        b'A' + value - 10
    }
}

pub fn crc16_ccitt_false(bytes: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in bytes {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands() -> [E2eCommand; 12] {
        [
            E2eCommand::Hello,
            E2eCommand::ReadTimestamp {
                target_sequence: 0x1234_5678,
            },
            E2eCommand::EnterDeviceDownload,
            E2eCommand::DropNextInput {
                lane: E2eInputLane::Critical,
            },
            E2eCommand::DropNextInputBurst {
                lane: E2eInputLane::Critical,
            },
            E2eCommand::SelectTransport {
                transport: InputTransport::EspNow,
            },
            E2eCommand::ReleaseAll,
            E2eCommand::Keyboard {
                modifiers: 0x22,
                keys: [4, 5, 6, 0, 0, 0],
            },
            E2eCommand::Mouse {
                buttons: 3,
                x: -1234,
                y: 2048,
                wheel: -2,
                pan: 3,
            },
            E2eCommand::Consumer { usage: 0x00e9 },
            E2eCommand::VendorInput {
                len: 63,
                seed: 0xa5,
            },
            E2eCommand::MouseBurst {
                count: 48,
                x: 1,
                y: -1,
            },
        ]
    }

    #[test]
    fn only_hello_requests_a_synchronous_acknowledgement() {
        for command in commands() {
            let packet = E2ePacket {
                sequence: 7,
                command,
            };
            assert_eq!(
                packet.requests_acknowledgement(),
                matches!(
                    command,
                    E2eCommand::Hello
                        | E2eCommand::EnterDeviceDownload
                        | E2eCommand::SelectTransport { .. }
                )
            );
        }
    }

    #[test]
    fn timestamp_query_is_not_an_input_event() {
        for command in commands() {
            let packet = E2ePacket {
                sequence: 7,
                command,
            };
            assert_eq!(
                packet.carries_input(),
                !matches!(
                    command,
                    E2eCommand::Hello
                        | E2eCommand::ReadTimestamp { .. }
                        | E2eCommand::EnterDeviceDownload
                        | E2eCommand::SelectTransport { .. }
                )
            );
        }
    }

    #[test]
    fn every_command_round_trips_binary_and_uart_line() {
        for (index, command) in commands().into_iter().enumerate() {
            let packet = E2ePacket {
                sequence: 0x1020_3040 + index as u32,
                command,
            };
            assert_eq!(E2ePacket::decode(&packet.encode()), Ok(packet));
            assert_eq!(E2ePacket::decode_line(&packet.encode_line()), Ok(packet));
        }
    }

    #[test]
    fn corruption_and_non_e2e_logs_are_rejected() {
        let packet = E2ePacket {
            sequence: 1,
            command: E2eCommand::Hello,
        };
        let mut encoded = packet.encode();
        encoded[6] ^= 1;
        assert_eq!(
            E2ePacket::decode(&encoded),
            Err(E2eProtocolError::InvalidChecksum)
        );
        assert_eq!(
            E2ePacket::decode_line(b"firmware: unrelated log"),
            Err(E2eProtocolError::InvalidLength)
        );
    }

    #[test]
    fn packets_convert_at_the_normalized_input_boundary() {
        let packet = E2ePacket {
            sequence: 9,
            command: E2eCommand::Keyboard {
                modifiers: ModifierState::LEFT_SHIFT.bits(),
                keys: [4, 5, 0, 0, 0, 0],
            },
        };
        let frames = packet.input_frames().unwrap();
        let Some(InputFrame::Standard(frame)) = &frames[0] else {
            panic!("keyboard frame missing")
        };
        let keyboard = frame.keyboard.as_ref().unwrap();
        assert_eq!(keyboard.modifiers, ModifierState::LEFT_SHIFT);
        assert_eq!(keyboard.keys_down(), &[KeyUsage(4), KeyUsage(5)]);
    }

    #[test]
    fn release_all_clears_keyboard_mouse_and_consumer_domains() {
        let frames = E2ePacket {
            sequence: 10,
            command: E2eCommand::ReleaseAll,
        }
        .input_frames()
        .unwrap();
        assert!(frames.iter().all(Option::is_some));
    }
}
