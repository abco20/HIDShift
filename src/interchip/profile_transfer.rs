use crate::checksum::crc32_ieee;
use crate::mirror::{HSMI_MAX_SIZE, MirrorRejectReason, validate_mirror_image};

use super::{
    PROFILE_CHUNK_MAX_DATA_LEN, ProfileBegin, ProfileChunk, ProfileChunkData, ProfileResult,
    ProfileResultStatus,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileTransferCommand {
    Begin(ProfileBegin),
    Chunk(ProfileChunkData),
    Commit { transfer_id: u32 },
}

pub struct ProfileTransferEncoder<'a> {
    image: &'a [u8],
    begin: ProfileBegin,
    offset: usize,
    phase: u8,
}

impl<'a> ProfileTransferEncoder<'a> {
    pub fn new(
        transfer_id: u32,
        profile_hash: u32,
        image: &'a [u8],
    ) -> Result<Self, ProfileTransferError> {
        if image.is_empty() || image.len() > HSMI_MAX_SIZE {
            return Err(ProfileTransferError::InvalidLength);
        }
        Ok(Self {
            image,
            begin: ProfileBegin {
                transfer_id,
                total_length: image.len() as u32,
                crc32: crc32_ieee(image),
                profile_hash,
            },
            offset: 0,
            phase: 0,
        })
    }
}

impl Iterator for ProfileTransferEncoder<'_> {
    type Item = ProfileTransferCommand;

    fn next(&mut self) -> Option<Self::Item> {
        if self.phase == 0 {
            self.phase = 1;
            return Some(ProfileTransferCommand::Begin(self.begin));
        }
        if self.offset < self.image.len() {
            let end = (self.offset + PROFILE_CHUNK_MAX_DATA_LEN).min(self.image.len());
            let chunk = ProfileChunkData::from_borrowed(ProfileChunk {
                transfer_id: self.begin.transfer_id,
                offset: self.offset as u32,
                data: &self.image[self.offset..end],
            })
            .ok()?;
            self.offset = end;
            return Some(ProfileTransferCommand::Chunk(chunk));
        }
        if self.phase == 1 {
            self.phase = 2;
            return Some(ProfileTransferCommand::Commit {
                transfer_id: self.begin.transfer_id,
            });
        }
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransferState {
    begin: ProfileBegin,
    next_offset: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommittedProfile {
    pub transfer_id: u32,
    pub profile_hash: u32,
    pub length: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileChunkDisposition {
    Appended,
    Duplicate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileTransferError {
    Busy,
    InvalidLength,
    WrongTransfer,
    OffsetGap,
    ConflictingRetry,
}

pub struct ProfileTransferReceiver<'a> {
    staging: &'a mut [u8],
    active: Option<TransferState>,
    committed: Option<CommittedProfile>,
}

impl<'a> ProfileTransferReceiver<'a> {
    pub fn new(staging: &'a mut [u8]) -> Self {
        Self {
            staging,
            active: None,
            committed: None,
        }
    }

    pub fn begin(&mut self, begin: ProfileBegin) -> Result<(), ProfileTransferError> {
        if self.active.is_some() || self.committed.is_some() {
            return Err(ProfileTransferError::Busy);
        }
        let total_length =
            usize::try_from(begin.total_length).map_err(|_| ProfileTransferError::InvalidLength)?;
        if total_length == 0 || total_length > HSMI_MAX_SIZE || total_length > self.staging.len() {
            return Err(ProfileTransferError::InvalidLength);
        }
        self.active = Some(TransferState {
            begin,
            next_offset: 0,
        });
        Ok(())
    }

    pub fn chunk(
        &mut self,
        chunk: ProfileChunk<'_>,
    ) -> Result<ProfileChunkDisposition, ProfileTransferError> {
        let Some(state) = self.active else {
            return Err(ProfileTransferError::WrongTransfer);
        };
        if chunk.transfer_id != state.begin.transfer_id {
            return Err(ProfileTransferError::WrongTransfer);
        }
        if chunk.data.is_empty() {
            self.active = None;
            return Err(ProfileTransferError::InvalidLength);
        }
        let offset = usize::try_from(chunk.offset).map_err(|_| {
            self.active = None;
            ProfileTransferError::InvalidLength
        })?;
        let end = offset.checked_add(chunk.data.len()).ok_or_else(|| {
            self.active = None;
            ProfileTransferError::InvalidLength
        })?;
        let total_length = state.begin.total_length as usize;
        if end > total_length {
            self.active = None;
            return Err(ProfileTransferError::InvalidLength);
        }
        if offset > state.next_offset {
            self.active = None;
            return Err(ProfileTransferError::OffsetGap);
        }
        if offset < state.next_offset {
            if end <= state.next_offset && self.staging[offset..end] == *chunk.data {
                return Ok(ProfileChunkDisposition::Duplicate);
            }
            self.active = None;
            return Err(ProfileTransferError::ConflictingRetry);
        }

        self.staging[offset..end].copy_from_slice(chunk.data);
        self.active = Some(TransferState {
            next_offset: end,
            ..state
        });
        Ok(ProfileChunkDisposition::Appended)
    }

    pub fn commit(&mut self, transfer_id: u32) -> ProfileResult {
        let Some(state) = self.active.take() else {
            return failure_result(
                transfer_id,
                0,
                ProfileResultStatus::InvalidImage,
                MirrorRejectReason::MalformedImage,
            );
        };
        if state.begin.transfer_id != transfer_id {
            return failure_result(
                transfer_id,
                state.begin.profile_hash,
                ProfileResultStatus::Busy,
                MirrorRejectReason::MalformedImage,
            );
        }
        let length = state.begin.total_length as usize;
        if state.next_offset != length || crc32_ieee(&self.staging[..length]) != state.begin.crc32 {
            return failure_result(
                transfer_id,
                state.begin.profile_hash,
                ProfileResultStatus::InvalidImage,
                MirrorRejectReason::MalformedImage,
            );
        }
        if let Err(reason) = validate_mirror_image(&self.staging[..length]) {
            let status = match reason {
                MirrorRejectReason::EndpointResourceExhausted => {
                    ProfileResultStatus::ResourceExhausted
                }
                MirrorRejectReason::NonHidInterface
                | MirrorRejectReason::UnsupportedAlternateSetting
                | MirrorRejectReason::UnsupportedEndpointType
                | MirrorRejectReason::PacketTooLarge
                | MirrorRejectReason::UnsupportedUsbVersion
                | MirrorRejectReason::RemoteWakeUnsupported => ProfileResultStatus::Unsupported,
                _ => ProfileResultStatus::InvalidImage,
            };
            return failure_result(transfer_id, state.begin.profile_hash, status, reason);
        }

        self.committed = Some(CommittedProfile {
            transfer_id,
            profile_hash: state.begin.profile_hash,
            length,
        });
        ProfileResult {
            transfer_id,
            profile_hash: state.begin.profile_hash,
            status: ProfileResultStatus::Accepted,
            reject_reason: MirrorRejectReason::None as u8,
            detail: 0,
        }
    }

    pub fn committed(&self) -> Option<(CommittedProfile, &[u8])> {
        let committed = self.committed?;
        Some((committed, &self.staging[..committed.length]))
    }

    pub fn clear_committed(&mut self) {
        self.committed = None;
    }

    pub fn cancel(&mut self) {
        self.active = None;
    }
}

fn failure_result(
    transfer_id: u32,
    profile_hash: u32,
    status: ProfileResultStatus,
    reason: MirrorRejectReason,
) -> ProfileResult {
    ProfileResult {
        transfer_id,
        profile_hash,
        status,
        reject_reason: reason as u8,
        detail: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fallback::build_fallback_mirror_image;
    use crate::interchip::PROFILE_CHUNK_MAX_DATA_LEN;

    fn fallback_image() -> ([u8; 1024], usize) {
        let mut image = [0; 1024];
        let length = build_fallback_mirror_image(&mut image).unwrap();
        (image, length)
    }

    fn begin_for(image: &[u8], transfer_id: u32) -> ProfileBegin {
        ProfileBegin {
            transfer_id,
            total_length: image.len() as u32,
            crc32: crc32_ieee(image),
            profile_hash: 0x1234_5678,
        }
    }

    fn transfer_all(receiver: &mut ProfileTransferReceiver<'_>, image: &[u8], transfer_id: u32) {
        receiver.begin(begin_for(image, transfer_id)).unwrap();
        for (index, data) in image.chunks(PROFILE_CHUNK_MAX_DATA_LEN).enumerate() {
            receiver
                .chunk(ProfileChunk {
                    transfer_id,
                    offset: (index * PROFILE_CHUNK_MAX_DATA_LEN) as u32,
                    data,
                })
                .unwrap();
        }
    }

    #[test]
    fn complete_valid_profile_is_exposed_only_after_commit() {
        let (image, length) = fallback_image();
        let mut staging = [0; HSMI_MAX_SIZE];
        let mut receiver = ProfileTransferReceiver::new(&mut staging);
        transfer_all(&mut receiver, &image[..length], 7);

        assert!(receiver.committed().is_none());
        let result = receiver.commit(7);
        assert_eq!(result.status, ProfileResultStatus::Accepted);
        let (metadata, committed) = receiver.committed().unwrap();
        assert_eq!(metadata.length, length);
        assert_eq!(committed, &image[..length]);
    }

    #[test]
    fn identical_chunk_retry_is_idempotent_but_conflict_discards_transfer() {
        let (image, length) = fallback_image();
        let mut staging = [0; HSMI_MAX_SIZE];
        let mut receiver = ProfileTransferReceiver::new(&mut staging);
        receiver.begin(begin_for(&image[..length], 8)).unwrap();
        let chunk = ProfileChunk {
            transfer_id: 8,
            offset: 0,
            data: &image[..32],
        };
        assert_eq!(receiver.chunk(chunk), Ok(ProfileChunkDisposition::Appended));
        assert_eq!(
            receiver.chunk(chunk),
            Ok(ProfileChunkDisposition::Duplicate)
        );
        let mut conflicting = image[..32].to_vec();
        conflicting[0] ^= 1;
        assert_eq!(
            receiver.chunk(ProfileChunk {
                data: &conflicting,
                ..chunk
            }),
            Err(ProfileTransferError::ConflictingRetry)
        );
        assert_eq!(receiver.commit(8).status, ProfileResultStatus::InvalidImage);
    }

    #[test]
    fn offset_gap_and_crc_mismatch_never_publish_staging_data() {
        let (image, length) = fallback_image();
        let mut staging = [0; HSMI_MAX_SIZE];
        let mut receiver = ProfileTransferReceiver::new(&mut staging);
        receiver.begin(begin_for(&image[..length], 9)).unwrap();
        assert_eq!(
            receiver.chunk(ProfileChunk {
                transfer_id: 9,
                offset: 1,
                data: &image[1..10],
            }),
            Err(ProfileTransferError::OffsetGap)
        );
        assert!(receiver.committed().is_none());

        let mut bad_begin = begin_for(&image[..length], 10);
        bad_begin.crc32 ^= 1;
        receiver.begin(bad_begin).unwrap();
        for (index, data) in image[..length]
            .chunks(PROFILE_CHUNK_MAX_DATA_LEN)
            .enumerate()
        {
            receiver
                .chunk(ProfileChunk {
                    transfer_id: 10,
                    offset: (index * PROFILE_CHUNK_MAX_DATA_LEN) as u32,
                    data,
                })
                .unwrap();
        }
        assert_eq!(
            receiver.commit(10).status,
            ProfileResultStatus::InvalidImage
        );
        assert!(receiver.committed().is_none());
    }

    #[test]
    fn sender_emits_begin_contiguous_chunks_and_commit() {
        let image = [0x5a; PROFILE_CHUNK_MAX_DATA_LEN * 2 + 1];
        let commands: std::vec::Vec<_> =
            ProfileTransferEncoder::new(4, 5, &image).unwrap().collect();
        assert!(matches!(commands[0], ProfileTransferCommand::Begin(_)));
        assert!(matches!(
            commands.last(),
            Some(ProfileTransferCommand::Commit { transfer_id: 4 })
        ));
        let chunks: std::vec::Vec<_> = commands
            .iter()
            .filter_map(|command| match command {
                ProfileTransferCommand::Chunk(chunk) => Some(chunk.as_borrowed()),
                _ => None,
            })
            .collect();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].offset, 0);
        assert_eq!(chunks[1].offset, PROFILE_CHUNK_MAX_DATA_LEN as u32);
        assert_eq!(chunks[2].data, &[0x5a]);
    }
}
