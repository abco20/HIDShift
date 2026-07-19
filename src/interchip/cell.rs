use crate::checksum::crc16_ccitt_false;

pub const SPI_CELL_LEN: usize = 128;
pub const SPI_CELL_HEADER_LEN: usize = 16;
pub const SPI_CELL_PAYLOAD_LEN: usize = 110;
pub const SPI_CELL_MAGIC: u16 = 0x4853;
pub const SPI_PROTOCOL_VERSION: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiCellHeader {
    pub magic: u16,
    pub version: u8,
    pub flags: u8,
    pub session_id: u32,
    pub tx_sequence: u16,
    pub cumulative_ack: u16,
    pub payload_len: u16,
    pub record_count: u8,
    pub receive_window: u8,
}

impl SpiCellHeader {
    pub const fn new(session_id: u32) -> Self {
        Self {
            magic: SPI_CELL_MAGIC,
            version: SPI_PROTOCOL_VERSION,
            flags: 0,
            session_id,
            tx_sequence: 0,
            cumulative_ack: 0,
            payload_len: 0,
            record_count: 0,
            receive_window: 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpiCell {
    pub header: SpiCellHeader,
    pub payload: [u8; SPI_CELL_PAYLOAD_LEN],
}

impl SpiCell {
    pub const fn empty(session_id: u32) -> Self {
        Self {
            header: SpiCellHeader::new(session_id),
            payload: [0; SPI_CELL_PAYLOAD_LEN],
        }
    }

    pub fn encode(&self) -> Result<[u8; SPI_CELL_LEN], SpiCellError> {
        let payload_len = self.header.payload_len as usize;
        if payload_len > SPI_CELL_PAYLOAD_LEN {
            return Err(SpiCellError::PayloadTooLong);
        }
        if self.header.tx_sequence == 0 && (payload_len != 0 || self.header.record_count != 0) {
            return Err(SpiCellError::SequenceZeroHasPayload);
        }
        let mut bytes = [0u8; SPI_CELL_LEN];
        bytes[0..2].copy_from_slice(&self.header.magic.to_le_bytes());
        bytes[2] = self.header.version;
        bytes[3] = self.header.flags;
        bytes[4..8].copy_from_slice(&self.header.session_id.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.header.tx_sequence.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.header.cumulative_ack.to_le_bytes());
        bytes[12..14].copy_from_slice(&self.header.payload_len.to_le_bytes());
        bytes[14] = self.header.record_count;
        bytes[15] = self.header.receive_window;
        bytes[SPI_CELL_HEADER_LEN..SPI_CELL_HEADER_LEN + payload_len]
            .copy_from_slice(&self.payload[..payload_len]);
        let crc = crc16_ccitt_false(&bytes[..SPI_CELL_LEN - 2]);
        bytes[SPI_CELL_LEN - 2..].copy_from_slice(&crc.to_le_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8; SPI_CELL_LEN]) -> Result<Self, SpiCellError> {
        let expected = u16::from_le_bytes([bytes[SPI_CELL_LEN - 2], bytes[SPI_CELL_LEN - 1]]);
        if crc16_ccitt_false(&bytes[..SPI_CELL_LEN - 2]) != expected {
            return Err(SpiCellError::CrcMismatch);
        }
        let header = SpiCellHeader {
            magic: u16::from_le_bytes([bytes[0], bytes[1]]),
            version: bytes[2],
            flags: bytes[3],
            session_id: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            tx_sequence: u16::from_le_bytes([bytes[8], bytes[9]]),
            cumulative_ack: u16::from_le_bytes([bytes[10], bytes[11]]),
            payload_len: u16::from_le_bytes([bytes[12], bytes[13]]),
            record_count: bytes[14],
            receive_window: bytes[15],
        };
        if header.magic != SPI_CELL_MAGIC {
            return Err(SpiCellError::InvalidMagic);
        }
        if header.version != SPI_PROTOCOL_VERSION {
            return Err(SpiCellError::UnsupportedVersion);
        }
        let payload_len = header.payload_len as usize;
        if payload_len > SPI_CELL_PAYLOAD_LEN {
            return Err(SpiCellError::PayloadTooLong);
        }
        if header.tx_sequence == 0 && (payload_len != 0 || header.record_count != 0) {
            return Err(SpiCellError::SequenceZeroHasPayload);
        }
        if bytes[SPI_CELL_HEADER_LEN + payload_len..SPI_CELL_LEN - 2]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(SpiCellError::NonZeroTrailingBytes);
        }
        let mut payload = [0; SPI_CELL_PAYLOAD_LEN];
        payload[..payload_len]
            .copy_from_slice(&bytes[SPI_CELL_HEADER_LEN..SPI_CELL_HEADER_LEN + payload_len]);
        Ok(Self { header, payload })
    }

    pub fn payload(&self) -> &[u8] {
        &self.payload[..self.header.payload_len as usize]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpiCellError {
    InvalidMagic,
    UnsupportedVersion,
    PayloadTooLong,
    SequenceZeroHasPayload,
    NonZeroTrailingBytes,
    CrcMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_round_trips_exact_fixed_wire_size() {
        let mut cell = SpiCell::empty(0x1234_5678);
        cell.header.tx_sequence = 7;
        cell.header.cumulative_ack = 5;
        cell.header.payload_len = 3;
        cell.header.record_count = 1;
        cell.payload[..3].copy_from_slice(&[1, 2, 3]);
        let bytes = cell.encode().unwrap();
        assert_eq!(bytes.len(), 128);
        assert_eq!(SpiCell::decode(&bytes), Ok(cell));
    }

    #[test]
    fn corruption_is_rejected_before_payload_processing() {
        let mut bytes = SpiCell::empty(9).encode().unwrap();
        bytes[40] ^= 0x80;
        assert_eq!(SpiCell::decode(&bytes), Err(SpiCellError::CrcMismatch));
    }

    #[test]
    fn sequence_zero_cannot_carry_records() {
        let mut cell = SpiCell::empty(1);
        cell.header.payload_len = 4;
        assert_eq!(cell.encode(), Err(SpiCellError::SequenceZeroHasPayload));
    }
}
