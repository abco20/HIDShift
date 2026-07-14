use aes::Aes128;
use aes::cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray};
use cmac::{Cmac, Mac};

use crate::espnow_pairing::{ESPNOW_KEY_LEN, EspNowRole};

pub const ESPNOW_FRAME_MAX: usize = 250;
pub const SECURE_HEADER_LEN: usize = 12;
pub const SECURE_TAG_LEN: usize = 8;
pub const SECURE_PAYLOAD_MAX: usize = ESPNOW_FRAME_MAX - SECURE_HEADER_LEN - SECURE_TAG_LEN;

const MAGIC: [u8; 2] = *b"HE";
const VERSION: u8 = 1;
const SESSION_KEY_LABEL: &[u8] = b"HIDSHIFT-ESPNOW-SESSION";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecureEspNowFrame {
    bytes: [u8; ESPNOW_FRAME_MAX],
    len: u8,
}

impl SecureEspNowFrame {
    pub fn seal(
        key: &[u8; ESPNOW_KEY_LEN],
        sender: EspNowRole,
        session: u32,
        sequence: u32,
        plaintext: &[u8],
    ) -> Result<Self, EspNowSecurityError> {
        if plaintext.len() > SECURE_PAYLOAD_MAX {
            return Err(EspNowSecurityError::PayloadTooLong);
        }
        let mut frame = Self {
            bytes: [0; ESPNOW_FRAME_MAX],
            len: (SECURE_HEADER_LEN + plaintext.len() + SECURE_TAG_LEN) as u8,
        };
        frame.bytes[..2].copy_from_slice(&MAGIC);
        frame.bytes[2] = VERSION;
        frame.bytes[3] = sender as u8;
        frame.bytes[4..8].copy_from_slice(&session.to_le_bytes());
        frame.bytes[8..12].copy_from_slice(&sequence.to_le_bytes());
        let payload_end = SECURE_HEADER_LEN + plaintext.len();
        frame.bytes[SECURE_HEADER_LEN..payload_end].copy_from_slice(plaintext);
        apply_ctr(
            key,
            sender,
            session,
            sequence,
            &mut frame.bytes[SECURE_HEADER_LEN..payload_end],
        );
        let tag = authentication_tag(key, &frame.bytes[..payload_end]);
        frame.bytes[payload_end..payload_end + SECURE_TAG_LEN].copy_from_slice(&tag);
        Ok(frame)
    }

    pub fn open<'a>(
        &'a mut self,
        key: &[u8; ESPNOW_KEY_LEN],
        expected_sender: EspNowRole,
    ) -> Result<SecureEspNowPayload<'a>, EspNowSecurityError> {
        let len = usize::from(self.len);
        if len < SECURE_HEADER_LEN + SECURE_TAG_LEN {
            return Err(EspNowSecurityError::TooShort);
        }
        if self.bytes[..2] != MAGIC {
            return Err(EspNowSecurityError::InvalidMagic);
        }
        if self.bytes[2] != VERSION {
            return Err(EspNowSecurityError::UnsupportedVersion);
        }
        if self.bytes[3] != expected_sender as u8 {
            return Err(EspNowSecurityError::WrongSenderRole);
        }
        let payload_end = len - SECURE_TAG_LEN;
        let expected_tag = authentication_tag(key, &self.bytes[..payload_end]);
        if !constant_time_eq(
            &expected_tag,
            &self.bytes[payload_end..payload_end + SECURE_TAG_LEN],
        ) {
            return Err(EspNowSecurityError::AuthenticationFailed);
        }
        let session = u32::from_le_bytes(self.bytes[4..8].try_into().unwrap_or([0; 4]));
        let sequence = u32::from_le_bytes(self.bytes[8..12].try_into().unwrap_or([0; 4]));
        apply_ctr(
            key,
            expected_sender,
            session,
            sequence,
            &mut self.bytes[SECURE_HEADER_LEN..payload_end],
        );
        Ok(SecureEspNowPayload {
            session,
            sequence,
            bytes: &self.bytes[SECURE_HEADER_LEN..payload_end],
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, EspNowSecurityError> {
        if bytes.len() > ESPNOW_FRAME_MAX {
            return Err(EspNowSecurityError::PayloadTooLong);
        }
        if bytes.len() < SECURE_HEADER_LEN + SECURE_TAG_LEN {
            return Err(EspNowSecurityError::TooShort);
        }
        let mut frame = Self {
            bytes: [0; ESPNOW_FRAME_MAX],
            len: bytes.len() as u8,
        };
        frame.bytes[..bytes.len()].copy_from_slice(bytes);
        Ok(frame)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Derive a directional key for an established host/device session pair.
/// Hello packets continue to use the pairing key so that a reboot can discover
/// the peer; all data packets use a key bound to both boot sessions and the
/// sender direction.
pub fn derive_session_key(
    pairing_key: &[u8; ESPNOW_KEY_LEN],
    host_session: u32,
    device_session: u32,
    sender: EspNowRole,
) -> [u8; ESPNOW_KEY_LEN] {
    let mut mac = <Cmac<Aes128> as KeyInit>::new(GenericArray::from_slice(pairing_key));
    mac.update(SESSION_KEY_LABEL);
    mac.update(&host_session.to_le_bytes());
    mac.update(&device_session.to_le_bytes());
    mac.update(&[sender as u8]);
    let full = mac.finalize().into_bytes();
    let mut key = [0; ESPNOW_KEY_LEN];
    key.copy_from_slice(&full[..ESPNOW_KEY_LEN]);
    key
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecureEspNowPayload<'a> {
    pub session: u32,
    pub sequence: u32,
    pub bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EspNowSecurityError {
    TooShort,
    PayloadTooLong,
    InvalidMagic,
    UnsupportedVersion,
    WrongSenderRole,
    AuthenticationFailed,
}

fn apply_ctr(
    key: &[u8; ESPNOW_KEY_LEN],
    role: EspNowRole,
    session: u32,
    sequence: u32,
    bytes: &mut [u8],
) {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    for (block_index, chunk) in bytes.chunks_mut(16).enumerate() {
        let mut counter = GenericArray::clone_from_slice(&[0; 16]);
        counter[..4].copy_from_slice(&session.to_le_bytes());
        counter[4..8].copy_from_slice(&sequence.to_le_bytes());
        counter[8] = role as u8;
        counter[9..13].copy_from_slice(&(block_index as u32).to_le_bytes());
        counter[13..16].copy_from_slice(b"HID");
        cipher.encrypt_block(&mut counter);
        for (byte, mask) in chunk.iter_mut().zip(counter.iter()) {
            *byte ^= *mask;
        }
    }
}

fn authentication_tag(key: &[u8; ESPNOW_KEY_LEN], bytes: &[u8]) -> [u8; SECURE_TAG_LEN] {
    let mut mac = <Cmac<Aes128> as KeyInit>::new(GenericArray::from_slice(key));
    mac.update(bytes);
    let full = mac.finalize().into_bytes();
    let mut tag = [0; SECURE_TAG_LEN];
    tag.copy_from_slice(&full[..SECURE_TAG_LEN]);
    tag
}

fn constant_time_eq(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 16] = *b"pairing-key-0001";

    #[test]
    fn authenticated_frame_round_trips_without_exposing_plaintext() {
        let plaintext = b"keyboard vendor report";
        let sealed =
            SecureEspNowFrame::seal(&KEY, EspNowRole::UsbHost, 0x1234, 7, plaintext).unwrap();
        assert!(
            !sealed
                .as_bytes()
                .windows(plaintext.len())
                .any(|w| w == plaintext)
        );
        let mut received = SecureEspNowFrame::decode(sealed.as_bytes()).unwrap();
        let opened = received.open(&KEY, EspNowRole::UsbHost).unwrap();
        assert_eq!(opened.bytes, plaintext);
        assert_eq!((opened.session, opened.sequence), (0x1234, 7));
    }

    #[test]
    fn tampering_wrong_key_and_reflected_direction_are_rejected() {
        let sealed = SecureEspNowFrame::seal(&KEY, EspNowRole::UsbHost, 1, 2, b"input").unwrap();
        let mut tampered = sealed.clone();
        tampered.bytes[SECURE_HEADER_LEN] ^= 1;
        assert_eq!(
            tampered.open(&KEY, EspNowRole::UsbHost),
            Err(EspNowSecurityError::AuthenticationFailed)
        );
        let mut wrong_key = sealed.clone();
        assert_eq!(
            wrong_key.open(&[0x55; 16], EspNowRole::UsbHost),
            Err(EspNowSecurityError::AuthenticationFailed)
        );
        let mut reflected = sealed;
        assert_eq!(
            reflected.open(&KEY, EspNowRole::UsbDevice),
            Err(EspNowSecurityError::WrongSenderRole)
        );
    }

    #[test]
    fn session_keys_are_directional_and_bound_to_both_boots() {
        let host_to_device = derive_session_key(&KEY, 10, 20, EspNowRole::UsbHost);
        let device_to_host = derive_session_key(&KEY, 10, 20, EspNowRole::UsbDevice);
        assert_ne!(host_to_device, device_to_host);
        assert_ne!(
            host_to_device,
            derive_session_key(&KEY, 11, 20, EspNowRole::UsbHost)
        );
        assert_ne!(
            host_to_device,
            derive_session_key(&KEY, 10, 21, EspNowRole::UsbHost)
        );

        let sealed =
            SecureEspNowFrame::seal(&host_to_device, EspNowRole::UsbHost, 10, 1, b"input").unwrap();
        let mut received = SecureEspNowFrame::decode(sealed.as_bytes()).unwrap();
        assert_eq!(
            received.open(&device_to_host, EspNowRole::UsbHost),
            Err(EspNowSecurityError::AuthenticationFailed)
        );
    }
}
