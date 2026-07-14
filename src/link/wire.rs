use bitflags::bitflags;

use super::MAX_BRIDGE_MESSAGE_SIZE;

pub const BRIDGE_LINK_PROTOCOL_VERSION: u8 = 2;
pub const ESP_NOW_PAYLOAD_MAX: usize = crate::espnow_security::SECURE_PAYLOAD_MAX;
pub const WIRE_HEADER_LEN: usize = 24;
pub const WIRE_PAYLOAD_MAX: usize = ESP_NOW_PAYLOAD_MAX - WIRE_HEADER_LEN;
const MAGIC: [u8; 2] = *b"HS";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PacketKind {
    Data = 1,
    Heartbeat = 3,
    Reset = 4,
}

impl PacketKind {
    fn decode(value: u8) -> Result<Self, WirePacketError> {
        match value {
            1 => Ok(Self::Data),
            3 => Ok(Self::Heartbeat),
            4 => Ok(Self::Reset),
            _ => Err(WirePacketError::UnknownKind),
        }
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct PacketFlags: u8 {
        const FIRST = 1 << 0;
        const LAST = 1 << 1;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WirePacket {
    bytes: [u8; ESP_NOW_PAYLOAD_MAX],
    len: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WirePacketView<'a> {
    pub kind: PacketKind,
    pub flags: PacketFlags,
    pub fragment_index: u8,
    pub fragment_count: u8,
    pub session: u32,
    pub sequence: u32,
    pub message_id: u32,
    pub message_len: u16,
    pub payload: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WirePacketError {
    TooShort,
    TooLong,
    PayloadTooLong,
    InvalidMagic,
    UnsupportedVersion,
    UnknownKind,
    InvalidFlags,
    InvalidFragment,
    InvalidMessageLength,
    InvalidChecksum,
}

impl WirePacket {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        kind: PacketKind,
        flags: PacketFlags,
        fragment_index: u8,
        fragment_count: u8,
        session: u32,
        sequence: u32,
        message_id: u32,
        message_len: u16,
        payload: &[u8],
    ) -> Result<Self, WirePacketError> {
        if payload.len() > WIRE_PAYLOAD_MAX {
            return Err(WirePacketError::PayloadTooLong);
        }
        validate_fragment(
            kind,
            flags,
            fragment_index,
            fragment_count,
            message_len,
            payload.len(),
        )?;
        let mut packet = Self {
            bytes: [0; ESP_NOW_PAYLOAD_MAX],
            len: (WIRE_HEADER_LEN + payload.len()) as u8,
        };
        packet.bytes[0..2].copy_from_slice(&MAGIC);
        packet.bytes[2] = BRIDGE_LINK_PROTOCOL_VERSION;
        packet.bytes[3] = kind as u8;
        packet.bytes[4] = flags.bits();
        packet.bytes[5] = fragment_index;
        packet.bytes[6] = fragment_count;
        packet.bytes[7] = payload.len() as u8;
        packet.bytes[8..12].copy_from_slice(&session.to_le_bytes());
        packet.bytes[12..16].copy_from_slice(&sequence.to_le_bytes());
        packet.bytes[16..20].copy_from_slice(&message_id.to_le_bytes());
        packet.bytes[20..22].copy_from_slice(&message_len.to_le_bytes());
        packet.bytes[WIRE_HEADER_LEN..WIRE_HEADER_LEN + payload.len()].copy_from_slice(payload);
        let checksum = crc16(&packet.bytes[..22], payload);
        packet.bytes[22..24].copy_from_slice(&checksum.to_le_bytes());
        Ok(packet)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }

    pub fn decode(bytes: &[u8]) -> Result<WirePacketView<'_>, WirePacketError> {
        if bytes.len() < WIRE_HEADER_LEN {
            return Err(WirePacketError::TooShort);
        }
        if bytes.len() > ESP_NOW_PAYLOAD_MAX {
            return Err(WirePacketError::TooLong);
        }
        if bytes[0..2] != MAGIC {
            return Err(WirePacketError::InvalidMagic);
        }
        if bytes[2] != BRIDGE_LINK_PROTOCOL_VERSION {
            return Err(WirePacketError::UnsupportedVersion);
        }
        let kind = PacketKind::decode(bytes[3])?;
        let flags = PacketFlags::from_bits(bytes[4]).ok_or(WirePacketError::InvalidFlags)?;
        let payload_len = bytes[7] as usize;
        if bytes.len() != WIRE_HEADER_LEN + payload_len {
            return Err(WirePacketError::InvalidMessageLength);
        }
        let payload = &bytes[WIRE_HEADER_LEN..];
        let expected = u16::from_le_bytes([bytes[22], bytes[23]]);
        if crc16(&bytes[..22], payload) != expected {
            return Err(WirePacketError::InvalidChecksum);
        }
        let message_len = u16::from_le_bytes([bytes[20], bytes[21]]);
        validate_fragment(kind, flags, bytes[5], bytes[6], message_len, payload_len)?;
        Ok(WirePacketView {
            kind,
            flags,
            fragment_index: bytes[5],
            fragment_count: bytes[6],
            session: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            sequence: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            message_id: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            message_len,
            payload,
        })
    }
}

fn validate_fragment(
    kind: PacketKind,
    flags: PacketFlags,
    fragment_index: u8,
    fragment_count: u8,
    message_len: u16,
    payload_len: usize,
) -> Result<(), WirePacketError> {
    if kind != PacketKind::Data {
        if fragment_index != 0 || fragment_count != 0 || message_len != 0 || payload_len != 0 {
            return Err(WirePacketError::InvalidFragment);
        }
        return Ok(());
    }
    if fragment_count == 0 || fragment_index >= fragment_count {
        return Err(WirePacketError::InvalidFragment);
    }
    if flags.contains(PacketFlags::FIRST) != (fragment_index == 0)
        || flags.contains(PacketFlags::LAST) != (fragment_index + 1 == fragment_count)
    {
        return Err(WirePacketError::InvalidFragment);
    }
    if message_len == 0 || message_len as usize > MAX_BRIDGE_MESSAGE_SIZE {
        return Err(WirePacketError::InvalidMessageLength);
    }
    let expected_offset = fragment_index as usize * WIRE_PAYLOAD_MAX;
    if expected_offset + payload_len > message_len as usize {
        return Err(WirePacketError::InvalidMessageLength);
    }
    if !flags.contains(PacketFlags::LAST) && payload_len != WIRE_PAYLOAD_MAX {
        return Err(WirePacketError::InvalidFragment);
    }
    if flags.contains(PacketFlags::LAST) && expected_offset + payload_len != message_len as usize {
        return Err(WirePacketError::InvalidMessageLength);
    }
    Ok(())
}

pub struct FragmentEncoder<'a> {
    message: &'a [u8],
    session: u32,
    next_sequence: u32,
    message_id: u32,
    fragment_index: u8,
    fragment_count: u8,
}

impl<'a> FragmentEncoder<'a> {
    pub fn new(
        message: &'a [u8],
        session: u32,
        first_sequence: u32,
        message_id: u32,
    ) -> Result<Self, WirePacketError> {
        if message.is_empty() || message.len() > MAX_BRIDGE_MESSAGE_SIZE {
            return Err(WirePacketError::InvalidMessageLength);
        }
        let fragment_count = message.len().div_ceil(WIRE_PAYLOAD_MAX);
        Ok(Self {
            message,
            session,
            next_sequence: first_sequence,
            message_id,
            fragment_index: 0,
            fragment_count: fragment_count as u8,
        })
    }

    pub const fn fragment_count(&self) -> u8 {
        self.fragment_count
    }
}

impl Iterator for FragmentEncoder<'_> {
    type Item = WirePacket;

    fn next(&mut self) -> Option<Self::Item> {
        if self.fragment_index >= self.fragment_count {
            return None;
        }
        let offset = self.fragment_index as usize * WIRE_PAYLOAD_MAX;
        let end = (offset + WIRE_PAYLOAD_MAX).min(self.message.len());
        let mut flags = PacketFlags::empty();
        if self.fragment_index == 0 {
            flags |= PacketFlags::FIRST;
        }
        if self.fragment_index + 1 == self.fragment_count {
            flags |= PacketFlags::LAST;
        }
        let packet = WirePacket::new(
            PacketKind::Data,
            flags,
            self.fragment_index,
            self.fragment_count,
            self.session,
            self.next_sequence,
            self.message_id,
            self.message.len() as u16,
            &self.message[offset..end],
        )
        .expect("fragment encoder maintains wire invariants");
        self.fragment_index += 1;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        Some(packet)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reassembler {
    session: Option<u32>,
    message_id: Option<u32>,
    message_len: usize,
    fragment_count: u8,
    received: u8,
    received_mask: u8,
    buffer: [u8; MAX_BRIDGE_MESSAGE_SIZE],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReassemblyError {
    Wire(WirePacketError),
    NotData,
    TooManyFragments,
    ConflictingMessage,
}

impl Reassembler {
    pub const fn new() -> Self {
        Self {
            session: None,
            message_id: None,
            message_len: 0,
            fragment_count: 0,
            received: 0,
            received_mask: 0,
            buffer: [0; MAX_BRIDGE_MESSAGE_SIZE],
        }
    }

    pub fn push(&mut self, bytes: &[u8]) -> Result<Option<&[u8]>, ReassemblyError> {
        let packet = WirePacket::decode(bytes).map_err(ReassemblyError::Wire)?;
        if packet.kind != PacketKind::Data {
            return Err(ReassemblyError::NotData);
        }
        if packet.fragment_count > 8 {
            return Err(ReassemblyError::TooManyFragments);
        }
        let new_message = self.session != Some(packet.session)
            || self.message_id != Some(packet.message_id)
            || self.received == self.fragment_count;
        if new_message {
            self.session = Some(packet.session);
            self.message_id = Some(packet.message_id);
            self.message_len = packet.message_len as usize;
            self.fragment_count = packet.fragment_count;
            self.received = 0;
            self.received_mask = 0;
        } else if self.message_len != packet.message_len as usize
            || self.fragment_count != packet.fragment_count
        {
            return Err(ReassemblyError::ConflictingMessage);
        }

        let bit = 1u8 << packet.fragment_index;
        if self.received_mask & bit == 0 {
            let offset = packet.fragment_index as usize * WIRE_PAYLOAD_MAX;
            self.buffer[offset..offset + packet.payload.len()].copy_from_slice(packet.payload);
            self.received_mask |= bit;
            self.received += 1;
        }
        if self.received == self.fragment_count {
            Ok(Some(&self.buffer[..self.message_len]))
        } else {
            Ok(None)
        }
    }
}

impl Default for Reassembler {
    fn default() -> Self {
        Self::new()
    }
}

fn crc16(header: &[u8], payload: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for byte in header.iter().chain(payload) {
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

    #[test]
    fn maximum_descriptor_message_fragments_and_reassembles_out_of_order() {
        let mut message = [0u8; MAX_BRIDGE_MESSAGE_SIZE];
        for (index, byte) in message.iter_mut().enumerate() {
            *byte = index as u8;
        }
        let fragments: heapless::Vec<_, 8> = FragmentEncoder::new(&message, 7, u32::MAX - 2, 42)
            .unwrap()
            .collect();
        assert_eq!(fragments.len(), message.len().div_ceil(WIRE_PAYLOAD_MAX));

        let mut reassembler = Reassembler::new();
        for index in (1..fragments.len()).rev() {
            assert_eq!(reassembler.push(fragments[index].as_bytes()), Ok(None));
        }
        assert_eq!(
            reassembler.push(fragments[0].as_bytes()),
            Ok(Some(message.as_slice()))
        );
    }

    #[test]
    fn checksum_detects_corruption_before_hid_delivery() {
        let packet = FragmentEncoder::new(&[1, 2, 3], 1, 2, 3)
            .unwrap()
            .next()
            .unwrap();
        let mut corrupt = [0; ESP_NOW_PAYLOAD_MAX];
        corrupt[..packet.as_bytes().len()].copy_from_slice(packet.as_bytes());
        corrupt[WIRE_HEADER_LEN] ^= 0x80;
        assert_eq!(
            WirePacket::decode(&corrupt[..packet.as_bytes().len()]),
            Err(WirePacketError::InvalidChecksum)
        );
    }

    #[test]
    fn duplicate_fragment_is_idempotent() {
        let message = [0x55; WIRE_PAYLOAD_MAX + 3];
        let fragments: heapless::Vec<_, 8> =
            FragmentEncoder::new(&message, 1, 1, 1).unwrap().collect();
        let mut reassembler = Reassembler::new();
        assert_eq!(reassembler.push(fragments[0].as_bytes()), Ok(None));
        assert_eq!(reassembler.push(fragments[0].as_bytes()), Ok(None));
        assert_eq!(
            reassembler.push(fragments[1].as_bytes()),
            Ok(Some(message.as_slice()))
        );
    }

    #[test]
    fn data_fragments_only_carry_boundary_flags() {
        let packet = FragmentEncoder::new(&[1, 2, 3], 1, 2, 3)
            .unwrap()
            .next()
            .unwrap();
        let decoded = WirePacket::decode(packet.as_bytes()).unwrap();
        assert!(decoded.flags.contains(PacketFlags::FIRST));
        assert!(decoded.flags.contains(PacketFlags::LAST));
        assert_eq!(decoded.flags.bits(), 0b11);
    }
}
