use crate::checksum::{Crc32, crc32_ieee};

use super::image::HSMI_MAX_SIZE;

pub const MIRROR_PROFILE_PARTITION_LEN: usize = 0x1_0000;
const METADATA_SECTOR_LEN: usize = 0x1000;
const PROFILE_SLOT_LEN: usize = 0x6000;
const METADATA_A_OFFSET: usize = 0x0000;
const METADATA_B_OFFSET: usize = 0x1000;
const PROFILE_A_OFFSET: usize = 0x2000;
const PROFILE_B_OFFSET: usize = 0x8000;
const METADATA_LEN: usize = 32;
const METADATA_MAGIC: [u8; 4] = *b"HSPF";
const METADATA_VERSION: u16 = 1;
const COMMIT_MARKER: u32 = 0x4853_434d;

pub trait ProfileStoreBackend {
    type Error;

    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), Self::Error>;
    fn erase(&mut self, offset: usize, length: usize) -> Result<(), Self::Error>;
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileSlot {
    A,
    B,
}

impl ProfileSlot {
    const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    const fn metadata_offset(self) -> usize {
        match self {
            Self::A => METADATA_A_OFFSET,
            Self::B => METADATA_B_OFFSET,
        }
    }

    const fn profile_offset(self) -> usize {
        match self {
            Self::A => PROFILE_A_OFFSET,
            Self::B => PROFILE_B_OFFSET,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredProfile {
    pub slot: ProfileSlot,
    pub generation: u32,
    pub profile_hash: u32,
    pub length: usize,
    pub crc32: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileCommitOutcome {
    Stored(StoredProfile),
    AlreadyStored(StoredProfile),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileStoreError<E> {
    Backend(E),
    ImageTooLarge,
    ReadBackMismatch,
}

pub struct ProfileStore<B> {
    backend: B,
}

impl<B: ProfileStoreBackend> ProfileStore<B> {
    pub const fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn active(&mut self) -> Result<Option<StoredProfile>, ProfileStoreError<B::Error>> {
        let a = self.read_metadata(ProfileSlot::A)?;
        let b = self.read_metadata(ProfileSlot::B)?;
        Ok(match (a, b) {
            (Some(a), Some(b)) => Some(
                if generation_is_newer_or_equal(a.generation, b.generation) {
                    a
                } else {
                    b
                },
            ),
            (Some(profile), None) | (None, Some(profile)) => Some(profile),
            (None, None) => None,
        })
    }

    pub fn find(
        &mut self,
        profile_hash: u32,
    ) -> Result<Option<StoredProfile>, ProfileStoreError<B::Error>> {
        let a = self.read_metadata(ProfileSlot::A)?;
        let b = self.read_metadata(ProfileSlot::B)?;
        Ok([a, b]
            .into_iter()
            .flatten()
            .find(|profile| profile.profile_hash == profile_hash))
    }

    pub fn read_profile(
        &mut self,
        profile: StoredProfile,
        out: &mut [u8],
    ) -> Result<(), ProfileStoreError<B::Error>> {
        if out.len() < profile.length {
            return Err(ProfileStoreError::ImageTooLarge);
        }
        self.backend
            .read(profile.slot.profile_offset(), &mut out[..profile.length])
            .map_err(ProfileStoreError::Backend)?;
        if crc32_ieee(&out[..profile.length]) != profile.crc32 {
            return Err(ProfileStoreError::ReadBackMismatch);
        }
        Ok(())
    }

    pub fn commit(
        &mut self,
        image: &[u8],
        profile_hash: u32,
    ) -> Result<ProfileCommitOutcome, ProfileStoreError<B::Error>> {
        if image.is_empty() || image.len() > HSMI_MAX_SIZE || image.len() > PROFILE_SLOT_LEN {
            return Err(ProfileStoreError::ImageTooLarge);
        }
        let active = self.active()?;
        let image_crc = crc32_ieee(image);
        if let Some(active) = active
            && active.profile_hash == profile_hash
            && active.length == image.len()
            && active.crc32 == image_crc
        {
            return Ok(ProfileCommitOutcome::AlreadyStored(active));
        }
        let slot = active.map_or(ProfileSlot::A, |profile| profile.slot.other());
        let generation = active.map_or(1, |profile| profile.generation.wrapping_add(1));

        self.backend
            .erase(slot.profile_offset(), PROFILE_SLOT_LEN)
            .map_err(ProfileStoreError::Backend)?;
        self.backend
            .write(slot.profile_offset(), image)
            .map_err(ProfileStoreError::Backend)?;
        let mut crc = Crc32::new();
        let mut readback = [0; 256];
        let mut offset = 0;
        while offset < image.len() {
            let length = (image.len() - offset).min(readback.len());
            self.backend
                .read(slot.profile_offset() + offset, &mut readback[..length])
                .map_err(ProfileStoreError::Backend)?;
            crc.update(&readback[..length]);
            offset += length;
        }
        if crc.finalize() != image_crc {
            return Err(ProfileStoreError::ReadBackMismatch);
        }

        let profile = StoredProfile {
            slot,
            generation,
            profile_hash,
            length: image.len(),
            crc32: image_crc,
        };
        let metadata = encode_metadata(profile, u32::MAX);
        self.backend
            .erase(slot.metadata_offset(), METADATA_SECTOR_LEN)
            .map_err(ProfileStoreError::Backend)?;
        self.backend
            .write(slot.metadata_offset(), &metadata[..28])
            .map_err(ProfileStoreError::Backend)?;
        self.backend
            .write(slot.metadata_offset() + 28, &COMMIT_MARKER.to_le_bytes())
            .map_err(ProfileStoreError::Backend)?;
        Ok(ProfileCommitOutcome::Stored(profile))
    }

    pub fn into_backend(self) -> B {
        self.backend
    }

    fn read_metadata(
        &mut self,
        slot: ProfileSlot,
    ) -> Result<Option<StoredProfile>, ProfileStoreError<B::Error>> {
        let mut bytes = [0; METADATA_LEN];
        self.backend
            .read(slot.metadata_offset(), &mut bytes)
            .map_err(ProfileStoreError::Backend)?;
        Ok(decode_metadata(slot, &bytes))
    }
}

fn encode_metadata(profile: StoredProfile, marker: u32) -> [u8; METADATA_LEN] {
    let mut bytes = [0xff; METADATA_LEN];
    bytes[0..4].copy_from_slice(&METADATA_MAGIC);
    bytes[4..6].copy_from_slice(&METADATA_VERSION.to_le_bytes());
    bytes[6..8].fill(0);
    bytes[8..12].copy_from_slice(&profile.generation.to_le_bytes());
    bytes[12..16].copy_from_slice(&profile.profile_hash.to_le_bytes());
    bytes[16..20].copy_from_slice(&(profile.length as u32).to_le_bytes());
    bytes[20..24].copy_from_slice(&profile.crc32.to_le_bytes());
    let metadata_crc = crc32_ieee(&bytes[..24]);
    bytes[24..28].copy_from_slice(&metadata_crc.to_le_bytes());
    bytes[28..32].copy_from_slice(&marker.to_le_bytes());
    bytes
}

fn decode_metadata(slot: ProfileSlot, bytes: &[u8; METADATA_LEN]) -> Option<StoredProfile> {
    if bytes[0..4] != METADATA_MAGIC
        || u16::from_le_bytes([bytes[4], bytes[5]]) != METADATA_VERSION
        || bytes[6..8] != [0, 0]
        || u32::from_le_bytes(bytes[28..32].try_into().ok()?) != COMMIT_MARKER
        || u32::from_le_bytes(bytes[24..28].try_into().ok()?) != crc32_ieee(&bytes[..24])
    {
        return None;
    }
    let length = u32::from_le_bytes(bytes[16..20].try_into().ok()?) as usize;
    if length == 0 || length > HSMI_MAX_SIZE || length > PROFILE_SLOT_LEN {
        return None;
    }
    Some(StoredProfile {
        slot,
        generation: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
        profile_hash: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
        length,
        crc32: u32::from_le_bytes(bytes[20..24].try_into().ok()?),
    })
}

const fn generation_is_newer_or_equal(left: u32, right: u32) -> bool {
    left.wrapping_sub(right) < 0x8000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone)]
    struct MemoryBackend {
        bytes: [u8; MIRROR_PROFILE_PARTITION_LEN],
        writes: u32,
    }

    impl MemoryBackend {
        const fn new() -> Self {
            Self {
                bytes: [0xff; MIRROR_PROFILE_PARTITION_LEN],
                writes: 0,
            }
        }
    }

    impl ProfileStoreBackend for MemoryBackend {
        type Error = ();

        fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), Self::Error> {
            out.copy_from_slice(&self.bytes[offset..offset + out.len()]);
            Ok(())
        }

        fn erase(&mut self, offset: usize, length: usize) -> Result<(), Self::Error> {
            self.bytes[offset..offset + length].fill(0xff);
            Ok(())
        }

        fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Self::Error> {
            for (destination, source) in
                self.bytes[offset..offset + data.len()].iter_mut().zip(data)
            {
                *destination &= *source;
            }
            self.writes += 1;
            Ok(())
        }
    }

    #[test]
    fn commits_alternate_slots_and_restores_newest_profile() {
        let mut store = ProfileStore::new(MemoryBackend::new());
        let first = match store.commit(b"profile-a", 1).unwrap() {
            ProfileCommitOutcome::Stored(profile) => profile,
            _ => panic!(),
        };
        let second = match store.commit(b"profile-b", 2).unwrap() {
            ProfileCommitOutcome::Stored(profile) => profile,
            _ => panic!(),
        };
        assert_eq!(first.slot, ProfileSlot::A);
        assert_eq!(second.slot, ProfileSlot::B);
        assert_eq!(store.active().unwrap(), Some(second));
        let mut readback = [0; 9];
        store.read_profile(second, &mut readback).unwrap();
        assert_eq!(&readback, b"profile-b");
        assert_eq!(store.find(1).unwrap(), Some(first));
        assert_eq!(store.find(2).unwrap(), Some(second));
        assert_eq!(store.find(3).unwrap(), None);
    }

    #[test]
    fn same_profile_hash_and_crc_skips_flash_write() {
        let mut store = ProfileStore::new(MemoryBackend::new());
        let stored = match store.commit(b"same", 7).unwrap() {
            ProfileCommitOutcome::Stored(profile) => profile,
            _ => panic!(),
        };
        let writes = store.backend.writes;
        assert_eq!(
            store.commit(b"same", 7),
            Ok(ProfileCommitOutcome::AlreadyStored(stored))
        );
        assert_eq!(store.backend.writes, writes);
    }

    #[test]
    fn missing_commit_marker_keeps_previous_slot_active() {
        let mut store = ProfileStore::new(MemoryBackend::new());
        let stored = match store.commit(b"valid", 1).unwrap() {
            ProfileCommitOutcome::Stored(profile) => profile,
            _ => panic!(),
        };
        let incomplete = StoredProfile {
            slot: ProfileSlot::B,
            generation: stored.generation + 1,
            profile_hash: 2,
            length: 7,
            crc32: crc32_ieee(b"partial"),
        };
        let metadata = encode_metadata(incomplete, u32::MAX);
        store.backend.bytes[METADATA_B_OFFSET..METADATA_B_OFFSET + METADATA_LEN]
            .copy_from_slice(&metadata);

        assert_eq!(store.active().unwrap(), Some(stored));
    }

    #[test]
    fn generation_selection_survives_wrap() {
        let mut backend = MemoryBackend::new();
        let old = StoredProfile {
            slot: ProfileSlot::A,
            generation: u32::MAX,
            profile_hash: 1,
            length: 1,
            crc32: 0,
        };
        let new = StoredProfile {
            slot: ProfileSlot::B,
            generation: 0,
            profile_hash: 2,
            length: 1,
            crc32: 0,
        };
        backend.bytes[METADATA_A_OFFSET..METADATA_A_OFFSET + METADATA_LEN]
            .copy_from_slice(&encode_metadata(old, COMMIT_MARKER));
        backend.bytes[METADATA_B_OFFSET..METADATA_B_OFFSET + METADATA_LEN]
            .copy_from_slice(&encode_metadata(new, COMMIT_MARKER));
        let mut store = ProfileStore::new(backend);

        assert_eq!(store.active().unwrap(), Some(new));
    }
}
