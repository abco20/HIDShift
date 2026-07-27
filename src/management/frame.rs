//! Transport-neutral management v1 envelope.
//!
//! BLE characteristics and USB CDC/UART carry the same logical frame. BLE may
//! split a frame into ATT-sized fragments; byte-stream transports use COBS
//! framing with a zero delimiter. Command payloads remain independent from the
//! framing and can evolve without changing transport adapters.

use heapless::Vec;

use crate::checksum::crc16_ccitt_false;

pub const FRAME_VERSION: u8 = 1;
pub const FRAME_PAYLOAD_CAPACITY: usize = 512;
pub const FRAME_HEADER_LEN: usize = 10;
pub const FRAME_CRC_LEN: usize = 2;
pub const FRAME_CAPACITY: usize = FRAME_HEADER_LEN + FRAME_PAYLOAD_CAPACITY + FRAME_CRC_LEN;
pub const STREAM_FRAME_CAPACITY: usize = FRAME_CAPACITY + FRAME_CAPACITY / 254 + 2;

const MAGIC: [u8; 2] = *b"HS";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FrameKind {
    Request = 1,
    Response = 2,
    Event = 3,
    Log = 4,
    Update = 5,
}

impl FrameKind {
    fn from_byte(value: u8) -> Result<Self, FrameError> {
        match value {
            1 => Ok(Self::Request),
            2 => Ok(Self::Response),
            3 => Ok(Self::Event),
            4 => Ok(Self::Log),
            5 => Ok(Self::Update),
            _ => Err(FrameError::InvalidKind),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum NodeId {
    Host = 1,
    Device = 2,
}

impl NodeId {
    fn from_byte(value: u8) -> Result<Self, FrameError> {
        match value {
            1 => Ok(Self::Host),
            2 => Ok(Self::Device),
            _ => Err(FrameError::InvalidNode),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    pub kind: FrameKind,
    pub request_id: u16,
    pub node: NodeId,
    pub flags: u8,
    pub payload: Vec<u8, FRAME_PAYLOAD_CAPACITY>,
}

impl Frame {
    pub fn new(
        kind: FrameKind,
        request_id: u16,
        node: NodeId,
        flags: u8,
        payload: &[u8],
    ) -> Result<Self, FrameError> {
        let mut owned = Vec::new();
        owned
            .extend_from_slice(payload)
            .map_err(|_| FrameError::PayloadTooLarge)?;
        Ok(Self {
            kind,
            request_id,
            node,
            flags,
            payload: owned,
        })
    }

    pub fn encode(&self) -> Vec<u8, FRAME_CAPACITY> {
        let mut bytes = Vec::new();
        let _ = bytes.extend_from_slice(&MAGIC);
        let _ = bytes.push(FRAME_VERSION);
        let _ = bytes.push(self.kind as u8);
        let _ = bytes.extend_from_slice(&self.request_id.to_le_bytes());
        let _ = bytes.push(self.node as u8);
        let _ = bytes.push(self.flags);
        let _ = bytes.extend_from_slice(&(self.payload.len() as u16).to_le_bytes());
        let _ = bytes.extend_from_slice(&self.payload);
        let crc = crc16_ccitt_false(&bytes);
        let _ = bytes.extend_from_slice(&crc.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < FRAME_HEADER_LEN + FRAME_CRC_LEN {
            return Err(FrameError::Truncated);
        }
        if bytes[..2] != MAGIC {
            return Err(FrameError::InvalidMagic);
        }
        if bytes[2] != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion);
        }
        let payload_len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
        if payload_len > FRAME_PAYLOAD_CAPACITY {
            return Err(FrameError::PayloadTooLarge);
        }
        if bytes.len() != FRAME_HEADER_LEN + payload_len + FRAME_CRC_LEN {
            return Err(FrameError::InvalidLength);
        }
        let body_end = bytes.len() - FRAME_CRC_LEN;
        let expected = u16::from_le_bytes([bytes[body_end], bytes[body_end + 1]]);
        if crc16_ccitt_false(&bytes[..body_end]) != expected {
            return Err(FrameError::CrcMismatch);
        }
        Self::new(
            FrameKind::from_byte(bytes[3])?,
            u16::from_le_bytes([bytes[4], bytes[5]]),
            NodeId::from_byte(bytes[6])?,
            bytes[7],
            &bytes[FRAME_HEADER_LEN..body_end],
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    Truncated,
    InvalidMagic,
    UnsupportedVersion,
    InvalidKind,
    InvalidNode,
    InvalidLength,
    PayloadTooLarge,
    CrcMismatch,
    MalformedCobs,
}

pub fn encode_stream(frame: &Frame) -> Vec<u8, STREAM_FRAME_CAPACITY> {
    let source = frame.encode();
    let mut encoded = Vec::new();
    let mut code_index = 0;
    let _ = encoded.push(0);
    let mut code = 1u8;
    for byte in source {
        if byte == 0 {
            encoded[code_index] = code;
            code_index = encoded.len();
            let _ = encoded.push(0);
            code = 1;
        } else {
            let _ = encoded.push(byte);
            code += 1;
            if code == 0xff {
                encoded[code_index] = code;
                code_index = encoded.len();
                let _ = encoded.push(0);
                code = 1;
            }
        }
    }
    encoded[code_index] = code;
    let _ = encoded.push(0);
    encoded
}

pub fn decode_stream(bytes: &[u8]) -> Result<Frame, FrameError> {
    let encoded = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    let mut decoded = Vec::<u8, FRAME_CAPACITY>::new();
    let mut index = 0;
    while index < encoded.len() {
        let code = encoded[index] as usize;
        if code == 0 || index + code > encoded.len() + 1 {
            return Err(FrameError::MalformedCobs);
        }
        index += 1;
        let end = index + code - 1;
        decoded
            .extend_from_slice(&encoded[index..end])
            .map_err(|_| FrameError::PayloadTooLarge)?;
        index = end;
        if code != 0xff && index < encoded.len() {
            decoded.push(0).map_err(|_| FrameError::PayloadTooLarge)?;
        }
    }
    Frame::decode(&decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variable_length_frames_round_trip_with_request_identity_and_node() {
        for length in [0, 1, 17, 128, FRAME_PAYLOAD_CAPACITY] {
            let payload = [0xa5; FRAME_PAYLOAD_CAPACITY];
            let frame = Frame::new(
                FrameKind::Response,
                0x4321,
                NodeId::Device,
                3,
                &payload[..length],
            )
            .unwrap();
            assert_eq!(Frame::decode(&frame.encode()), Ok(frame));
        }
    }

    #[test]
    fn cobs_stream_round_trip_handles_zero_bytes_and_delimiter() {
        let frame = Frame::new(FrameKind::Event, 0, NodeId::Host, 0, &[0, 1, 0, 2, 0]).unwrap();
        let encoded = encode_stream(&frame);
        assert_eq!(encoded.last(), Some(&0));
        assert!(!encoded[..encoded.len() - 1].contains(&0));
        assert_eq!(decode_stream(&encoded), Ok(frame));
    }

    #[test]
    fn corruption_is_rejected_before_payload_dispatch() {
        let frame = Frame::new(FrameKind::Request, 7, NodeId::Host, 0, b"status").unwrap();
        let mut bytes = frame.encode();
        bytes[FRAME_HEADER_LEN] ^= 0x80;
        assert_eq!(Frame::decode(&bytes), Err(FrameError::CrcMismatch));
    }

    #[test]
    fn malformed_lengths_and_oversized_payloads_are_explicit() {
        assert_eq!(
            Frame::new(
                FrameKind::Update,
                1,
                NodeId::Host,
                0,
                &[0; FRAME_PAYLOAD_CAPACITY + 1]
            ),
            Err(FrameError::PayloadTooLarge)
        );
        let frame = Frame::new(FrameKind::Request, 1, NodeId::Host, 0, &[]).unwrap();
        let mut bytes = frame.encode();
        bytes[8] = 1;
        assert_eq!(Frame::decode(&bytes), Err(FrameError::InvalidLength));
    }
}
