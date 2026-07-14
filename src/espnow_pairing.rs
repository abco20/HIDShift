pub const ESPNOW_KEY_LEN: usize = 16;
pub const ESPNOW_PAIRING_IMAGE_LEN: usize = 64;
pub const ESPNOW_PAIRING_SCHEMA_VERSION: u8 = 1;
pub const ESPNOW_PAIRING_KEY_CHUNK_LEN: usize = 14;

const MAGIC: [u8; 4] = *b"HSEP";
const CRC_OFFSET: usize = ESPNOW_PAIRING_IMAGE_LEN - 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum EspNowRole {
    UsbHost = 1,
    UsbDevice = 2,
}

impl EspNowRole {
    pub const fn peer(self) -> Self {
        match self {
            Self::UsbHost => Self::UsbDevice,
            Self::UsbDevice => Self::UsbHost,
        }
    }

    fn decode(value: u8) -> Result<Self, EspNowPairingError> {
        match value {
            1 => Ok(Self::UsbHost),
            2 => Ok(Self::UsbDevice),
            _ => Err(EspNowPairingError::InvalidRole),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EspNowPairing {
    pub generation: u32,
    pub local_role: EspNowRole,
    pub peer_address: [u8; 6],
    pub channel: u8,
    pub key: [u8; ESPNOW_KEY_LEN],
}

impl EspNowPairing {
    pub fn validate(&self) -> Result<(), EspNowPairingError> {
        if self.peer_address == [0; 6] || self.peer_address == [0xff; 6] {
            return Err(EspNowPairingError::InvalidPeerAddress);
        }
        if !(1..=14).contains(&self.channel) {
            return Err(EspNowPairingError::InvalidChannel);
        }
        if self.key == [0; ESPNOW_KEY_LEN] {
            return Err(EspNowPairingError::InvalidKey);
        }
        Ok(())
    }

    pub fn encode(self) -> Result<[u8; ESPNOW_PAIRING_IMAGE_LEN], EspNowPairingError> {
        self.validate()?;
        let mut image = [0xff; ESPNOW_PAIRING_IMAGE_LEN];
        image[..4].copy_from_slice(&MAGIC);
        image[4] = ESPNOW_PAIRING_SCHEMA_VERSION;
        image[5] = self.local_role as u8;
        image[6] = self.channel;
        image[8..12].copy_from_slice(&self.generation.to_le_bytes());
        image[12..18].copy_from_slice(&self.peer_address);
        image[18..34].copy_from_slice(&self.key);
        let crc = crc32(&image[..CRC_OFFSET]);
        image[CRC_OFFSET..].copy_from_slice(&crc.to_le_bytes());
        Ok(image)
    }

    pub fn decode(image: &[u8]) -> Result<Self, EspNowPairingError> {
        if image.len() != ESPNOW_PAIRING_IMAGE_LEN {
            return Err(EspNowPairingError::InvalidLength);
        }
        if image[..4] != MAGIC {
            return Err(EspNowPairingError::InvalidMagic);
        }
        if image[4] != ESPNOW_PAIRING_SCHEMA_VERSION {
            return Err(EspNowPairingError::UnsupportedVersion);
        }
        let expected = u32::from_le_bytes(
            image[CRC_OFFSET..]
                .try_into()
                .map_err(|_| EspNowPairingError::InvalidLength)?,
        );
        if crc32(&image[..CRC_OFFSET]) != expected {
            return Err(EspNowPairingError::CrcMismatch);
        }
        let pairing = Self {
            generation: u32::from_le_bytes(
                image[8..12]
                    .try_into()
                    .map_err(|_| EspNowPairingError::InvalidLength)?,
            ),
            local_role: EspNowRole::decode(image[5])?,
            channel: image[6],
            peer_address: image[12..18]
                .try_into()
                .map_err(|_| EspNowPairingError::InvalidLength)?,
            key: image[18..34]
                .try_into()
                .map_err(|_| EspNowPairingError::InvalidLength)?,
        };
        pairing.validate()?;
        Ok(pairing)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EspNowPairingTransaction {
    role: EspNowRole,
    peer_address: [u8; 6],
    channel: u8,
    key: [u8; ESPNOW_KEY_LEN],
    received: u16,
    active: bool,
}

impl EspNowPairingTransaction {
    pub const fn new(role: EspNowRole) -> Self {
        Self {
            role,
            peer_address: [0; 6],
            channel: 0,
            key: [0; ESPNOW_KEY_LEN],
            received: 0,
            active: false,
        }
    }

    pub fn begin(&mut self, peer_address: [u8; 6], channel: u8) -> Result<(), EspNowPairingError> {
        let candidate = EspNowPairing {
            generation: 0,
            local_role: self.role,
            peer_address,
            channel,
            key: [1; ESPNOW_KEY_LEN],
        };
        candidate.validate()?;
        self.peer_address = peer_address;
        self.channel = channel;
        self.key = [0; ESPNOW_KEY_LEN];
        self.received = 0;
        self.active = true;
        Ok(())
    }

    pub fn write_key_chunk(&mut self, offset: u8, bytes: &[u8]) -> Result<(), EspNowPairingError> {
        if !self.active {
            return Err(EspNowPairingError::NoTransaction);
        }
        if bytes.is_empty() || bytes.len() > ESPNOW_PAIRING_KEY_CHUNK_LEN {
            return Err(EspNowPairingError::InvalidKeyChunk);
        }
        let start = usize::from(offset);
        let end = start
            .checked_add(bytes.len())
            .filter(|end| *end <= ESPNOW_KEY_LEN)
            .ok_or(EspNowPairingError::InvalidKeyChunk)?;
        self.key[start..end].copy_from_slice(bytes);
        for index in start..end {
            self.received |= 1 << index;
        }
        Ok(())
    }

    pub fn commit(&mut self, generation: u32) -> Result<EspNowPairing, EspNowPairingError> {
        if !self.active {
            return Err(EspNowPairingError::NoTransaction);
        }
        if self.received != u16::MAX {
            return Err(EspNowPairingError::IncompleteKey);
        }
        let pairing = EspNowPairing {
            generation,
            local_role: self.role,
            peer_address: self.peer_address,
            channel: self.channel,
            key: self.key,
        };
        pairing.validate()?;
        self.cancel();
        Ok(pairing)
    }

    pub fn cancel(&mut self) {
        self.key = [0; ESPNOW_KEY_LEN];
        self.received = 0;
        self.active = false;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EspNowPairingError {
    InvalidLength,
    InvalidMagic,
    UnsupportedVersion,
    CrcMismatch,
    InvalidRole,
    InvalidPeerAddress,
    InvalidChannel,
    InvalidKey,
    InvalidKeyChunk,
    NoTransaction,
    IncompleteKey,
}

pub fn select_newest_pairing(
    first: &[u8; ESPNOW_PAIRING_IMAGE_LEN],
    second: &[u8; ESPNOW_PAIRING_IMAGE_LEN],
) -> Option<EspNowPairing> {
    match (EspNowPairing::decode(first), EspNowPairing::decode(second)) {
        (Ok(first), Ok(second)) => Some(
            if generation_is_newer(second.generation, first.generation) {
                second
            } else {
                first
            },
        ),
        (Ok(first), Err(_)) => Some(first),
        (Err(_), Ok(second)) => Some(second),
        (Err(_), Err(_)) => None,
    }
}

pub const fn generation_is_newer(candidate: u32, current: u32) -> bool {
    candidate != current && candidate.wrapping_sub(current) < 0x8000_0000
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    const PEER: [u8; 6] = [0x68, 0xee, 0x8f, 0x63, 0x94, 0xa0];
    const KEY: [u8; ESPNOW_KEY_LEN] = *b"pairing-key-0001";

    #[test]
    fn pairing_image_round_trips_and_detects_partial_writes() {
        let pairing = EspNowPairing {
            generation: 42,
            local_role: EspNowRole::UsbHost,
            peer_address: PEER,
            channel: 6,
            key: KEY,
        };
        let image = pairing.encode().unwrap();
        assert_eq!(EspNowPairing::decode(&image), Ok(pairing));

        let mut corrupt = image;
        corrupt[20] ^= 0x80;
        assert_eq!(
            EspNowPairing::decode(&corrupt),
            Err(EspNowPairingError::CrcMismatch)
        );
    }

    #[test]
    fn transaction_requires_every_key_byte_before_commit() {
        let mut transaction = EspNowPairingTransaction::new(EspNowRole::UsbHost);
        transaction.begin(PEER, 6).unwrap();
        transaction.write_key_chunk(0, &KEY[..14]).unwrap();
        assert_eq!(
            transaction.commit(1),
            Err(EspNowPairingError::IncompleteKey)
        );
        transaction.write_key_chunk(14, &KEY[14..]).unwrap();
        assert_eq!(transaction.commit(1).unwrap().key, KEY);
    }

    #[test]
    fn cancelled_or_invalid_pairing_cannot_be_committed() {
        let mut transaction = EspNowPairingTransaction::new(EspNowRole::UsbDevice);
        assert_eq!(
            transaction.begin([0xff; 6], 6),
            Err(EspNowPairingError::InvalidPeerAddress)
        );
        transaction.begin(PEER, 6).unwrap();
        transaction.write_key_chunk(0, &[0; 14]).unwrap();
        transaction.write_key_chunk(14, &[0; 2]).unwrap();
        assert_eq!(transaction.commit(1), Err(EspNowPairingError::InvalidKey));
        transaction.cancel();
        assert_eq!(
            transaction.commit(1),
            Err(EspNowPairingError::NoTransaction)
        );
    }

    #[test]
    fn newest_pairing_selection_survives_generation_wrap() {
        let pairing = |generation| EspNowPairing {
            generation,
            local_role: EspNowRole::UsbHost,
            peer_address: PEER,
            channel: 6,
            key: KEY,
        };
        let before_wrap = pairing(u32::MAX).encode().unwrap();
        let after_wrap = pairing(0).encode().unwrap();
        assert_eq!(
            select_newest_pairing(&before_wrap, &after_wrap),
            Some(pairing(0))
        );
    }
}
