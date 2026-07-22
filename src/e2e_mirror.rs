use crate::checksum::crc16_ccitt_false;

pub const MIRROR_E2E_VERSION: u8 = 1;
pub const MIRROR_E2E_PACKET_LEN: usize = 64;
pub const MIRROR_E2E_PAYLOAD_MAX: usize = 47;
pub const MIRROR_E2E_LINE_PREFIX: &[u8] = b"@HIDSHIFT-MIRROR:";
pub const MIRROR_E2E_LINE_LEN: usize = MIRROR_E2E_LINE_PREFIX.len() + MIRROR_E2E_PACKET_LEN * 2;

pub const OPCODE_HELLO: u8 = 0x01;
pub const OPCODE_REGISTER_BEGIN: u8 = 0x10;
pub const OPCODE_REGISTER_CHUNK: u8 = 0x11;
pub const OPCODE_REGISTER_COMMIT: u8 = 0x12;
pub const OPCODE_CLEAR_CANDIDATES: u8 = 0x13;
pub const OPCODE_INJECT_ENDPOINT_IN: u8 = 0x20;
pub const OPCODE_SET_CONTROL_RESPONSE: u8 = 0x21;
pub const OPCODE_READ_MOCK_STATUS: u8 = 0x22;
pub const OPCODE_RESET_MOCK_STATUS: u8 = 0x23;
pub const OPCODE_INJECT_SPI_CRC_FAILURE: u8 = 0x30;
pub const OPCODE_RESET_DEVICE_S3: u8 = 0x31;
pub const OPCODE_DROP_SPI_CELLS: u8 = 0x32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorE2ePacket {
    pub version: u8,
    pub opcode: u8,
    pub sequence: u32,
    pub transfer_id: u32,
    pub offset: u32,
    payload_len: u8,
    payload: [u8; MIRROR_E2E_PAYLOAD_MAX],
}

impl MirrorE2ePacket {
    pub fn new(
        opcode: u8,
        sequence: u32,
        transfer_id: u32,
        offset: u32,
        payload: &[u8],
    ) -> Result<Self, MirrorE2eError> {
        if payload.len() > MIRROR_E2E_PAYLOAD_MAX {
            return Err(MirrorE2eError::PayloadTooLarge);
        }
        let mut packet = Self {
            version: MIRROR_E2E_VERSION,
            opcode,
            sequence,
            transfer_id,
            offset,
            payload_len: payload.len() as u8,
            payload: [0; MIRROR_E2E_PAYLOAD_MAX],
        };
        packet.payload[..payload.len()].copy_from_slice(payload);
        Ok(packet)
    }

    pub const fn payload(&self) -> &[u8] {
        self.payload.split_at(self.payload_len as usize).0
    }

    pub fn encode(self) -> [u8; MIRROR_E2E_PACKET_LEN] {
        let mut bytes = [0; MIRROR_E2E_PACKET_LEN];
        bytes[0] = self.version;
        bytes[1] = self.opcode;
        bytes[2..6].copy_from_slice(&self.sequence.to_le_bytes());
        bytes[6..10].copy_from_slice(&self.transfer_id.to_le_bytes());
        bytes[10..14].copy_from_slice(&self.offset.to_le_bytes());
        bytes[14] = self.payload_len;
        bytes[15..62].copy_from_slice(&self.payload);
        let crc = crc16_ccitt_false(&bytes[..62]).to_le_bytes();
        bytes[62..64].copy_from_slice(&crc);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, MirrorE2eError> {
        if bytes.len() != MIRROR_E2E_PACKET_LEN {
            return Err(MirrorE2eError::InvalidLength);
        }
        if bytes[0] != MIRROR_E2E_VERSION {
            return Err(MirrorE2eError::UnsupportedVersion);
        }
        if u16::from_le_bytes([bytes[62], bytes[63]]) != crc16_ccitt_false(&bytes[..62]) {
            return Err(MirrorE2eError::CrcMismatch);
        }
        let payload_len = usize::from(bytes[14]);
        if payload_len > MIRROR_E2E_PAYLOAD_MAX
            || bytes[15 + payload_len..62].iter().any(|byte| *byte != 0)
        {
            return Err(MirrorE2eError::InvalidPayloadLength);
        }
        let mut payload = [0; MIRROR_E2E_PAYLOAD_MAX];
        payload.copy_from_slice(&bytes[15..62]);
        Ok(Self {
            version: bytes[0],
            opcode: bytes[1],
            sequence: read_u32(&bytes[2..6]),
            transfer_id: read_u32(&bytes[6..10]),
            offset: read_u32(&bytes[10..14]),
            payload_len: payload_len as u8,
            payload,
        })
    }

    pub fn decode_line(line: &[u8]) -> Result<Self, MirrorE2eError> {
        if line.len() != MIRROR_E2E_LINE_LEN || !line.starts_with(MIRROR_E2E_LINE_PREFIX) {
            return Err(MirrorE2eError::InvalidLength);
        }
        let mut bytes = [0; MIRROR_E2E_PACKET_LEN];
        let hex = &line[MIRROR_E2E_LINE_PREFIX.len()..];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = decode_hex(hex[index * 2], hex[index * 2 + 1])?;
        }
        Self::decode(&bytes)
    }

    pub fn encode_line(self) -> [u8; MIRROR_E2E_LINE_LEN] {
        let mut line = [0; MIRROR_E2E_LINE_LEN];
        line[..MIRROR_E2E_LINE_PREFIX.len()].copy_from_slice(MIRROR_E2E_LINE_PREFIX);
        for (index, byte) in self.encode().iter().copied().enumerate() {
            line[MIRROR_E2E_LINE_PREFIX.len() + index * 2] = hex_digit(byte >> 4);
            line[MIRROR_E2E_LINE_PREFIX.len() + index * 2 + 1] = hex_digit(byte & 0x0f);
        }
        line
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorE2eError {
    InvalidLength,
    UnsupportedVersion,
    PayloadTooLarge,
    InvalidPayloadLength,
    InvalidHex,
    CrcMismatch,
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

const fn hex_digit(value: u8) -> u8 {
    if value < 10 {
        b'0' + value
    } else {
        b'A' + value - 10
    }
}

const fn decode_hex(high: u8, low: u8) -> Result<u8, MirrorE2eError> {
    let Some(high) = hex_value(high) else {
        return Err(MirrorE2eError::InvalidHex);
    };
    let Some(low) = hex_value(low) else {
        return Err(MirrorE2eError::InvalidHex);
    };
    Ok(high << 4 | low)
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_and_uart_line_round_trip_exact_fixed_sizes() {
        let packet = MirrorE2ePacket::new(0x11, 2, 3, 47, &[1, 2, 3]).unwrap();
        assert_eq!(packet.encode().len(), 64);
        assert_eq!(packet.encode_line().len(), 145);
        assert_eq!(MirrorE2ePacket::decode(&packet.encode()), Ok(packet));
        assert_eq!(
            MirrorE2ePacket::decode_line(&packet.encode_line()),
            Ok(packet)
        );
    }

    #[test]
    fn corruption_and_noncanonical_tail_are_rejected() {
        let packet = MirrorE2ePacket::new(1, 2, 3, 4, &[5]).unwrap();
        let mut bytes = packet.encode();
        bytes[20] = 1;
        let crc = crc16_ccitt_false(&bytes[..62]).to_le_bytes();
        bytes[62..64].copy_from_slice(&crc);
        assert_eq!(
            MirrorE2ePacket::decode(&bytes),
            Err(MirrorE2eError::InvalidPayloadLength)
        );
        bytes[62] ^= 1;
        assert_eq!(
            MirrorE2ePacket::decode(&bytes),
            Err(MirrorE2eError::CrcMismatch)
        );
    }
}
