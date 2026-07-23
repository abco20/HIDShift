use crate::ids::DeviceId;
use crate::interchip::{MirrorControlRequest, MirrorControlResponse};
use crate::output_target::{MirrorCandidateId, MirrorStableId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MirrorCandidateMetadata {
    pub candidate: MirrorCandidateId,
    pub stable_id: MirrorStableId,
    pub profile_hash: u32,
    pub synthetic: bool,
    pub source_device: Option<DeviceId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorCandidateError {
    ImageUnavailable,
    EndpointOutRejected,
}

pub trait MirrorCandidateSource {
    fn metadata(&self) -> MirrorCandidateMetadata;

    fn mirror_image(&self) -> Result<&[u8], MirrorCandidateError>;

    fn handle_control_request(&mut self, request: MirrorControlRequest) -> MirrorControlResponse;

    fn handle_endpoint_out(
        &mut self,
        endpoint_address: u8,
        data: &[u8],
    ) -> Result<(), MirrorCandidateError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MirrorCandidateRegistryError {
    CandidateOutOfRange,
    AmbiguousIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MirrorCandidateRegistry<const N: usize> {
    entries: [Option<MirrorCandidateMetadata>; N],
}

impl<const N: usize> MirrorCandidateRegistry<N> {
    pub const fn new() -> Self {
        Self { entries: [None; N] }
    }

    pub fn register(
        &mut self,
        metadata: MirrorCandidateMetadata,
    ) -> Result<(), MirrorCandidateRegistryError> {
        let Some(entry) = self.entries.get_mut(usize::from(metadata.candidate.0)) else {
            return Err(MirrorCandidateRegistryError::CandidateOutOfRange);
        };
        *entry = Some(metadata);
        Ok(())
    }

    pub fn clear(
        &mut self,
        candidate: MirrorCandidateId,
    ) -> Result<(), MirrorCandidateRegistryError> {
        let Some(entry) = self.entries.get_mut(usize::from(candidate.0)) else {
            return Err(MirrorCandidateRegistryError::CandidateOutOfRange);
        };
        *entry = None;
        Ok(())
    }

    pub fn clear_source(&mut self, device_id: DeviceId) {
        for entry in &mut self.entries {
            if entry.is_some_and(|metadata| metadata.source_device == Some(device_id)) {
                *entry = None;
            }
        }
    }

    pub fn get(&self, candidate: MirrorCandidateId) -> Option<MirrorCandidateMetadata> {
        self.entries
            .get(usize::from(candidate.0))
            .copied()
            .flatten()
    }

    pub fn resolve(
        &self,
        stable_id: MirrorStableId,
    ) -> Result<Option<MirrorCandidateId>, MirrorCandidateRegistryError> {
        let mut resolved = None;
        for metadata in self.entries.iter().flatten() {
            if metadata.stable_id != stable_id {
                continue;
            }
            if resolved.is_some() {
                return Err(MirrorCandidateRegistryError::AmbiguousIdentity);
            }
            resolved = Some(metadata.candidate);
        }
        Ok(resolved)
    }
}

impl<const N: usize> Default for MirrorCandidateRegistry<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(candidate: u8, hash: u32) -> MirrorCandidateMetadata {
        let candidate = MirrorCandidateId(candidate);
        MirrorCandidateMetadata {
            candidate,
            stable_id: MirrorStableId::synthetic(hash),
            profile_hash: hash,
            synthetic: true,
            source_device: None,
        }
    }

    #[test]
    fn stable_identity_resolves_after_runtime_candidate_number_changes() {
        let selected = metadata(0, 0x1122_3344);
        let mut registry = MirrorCandidateRegistry::<4>::new();
        registry.register(metadata(2, 0x1122_3344)).unwrap();

        assert_eq!(
            registry.resolve(selected.stable_id),
            Ok(Some(MirrorCandidateId(2)))
        );
    }

    #[test]
    fn ambiguous_identity_is_never_guessed() {
        let mut registry = MirrorCandidateRegistry::<4>::new();
        let first = metadata(0, 0x1122_3344);
        let mut second = first;
        second.candidate = MirrorCandidateId(1);
        registry.register(first).unwrap();
        registry.register(second).unwrap();

        assert_eq!(
            registry.resolve(first.stable_id),
            Err(MirrorCandidateRegistryError::AmbiguousIdentity)
        );
    }
}
