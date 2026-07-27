#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareNode {
    Host,
    Device,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateTarget {
    Host,
    Device,
    Both,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageMetadata {
    pub node: FirmwareNode,
    pub size: u32,
    pub sha256: [u8; 32],
    pub version: [u8; 3],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareUpdatePhase {
    Idle,
    Staging,
    Ready,
    RebootPending,
    HealthCheck,
    Committed,
    RolledBack,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FirmwareUpdateError {
    InvalidPhase,
    WrongOffset,
    ImageTooLarge,
    HashMismatch,
    MissingNode,
    BundleMismatch,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FirmwareUpdate {
    phase: FirmwareUpdatePhase,
    target: UpdateTarget,
    bundle_id: [u8; 16],
    host: Option<ImageMetadata>,
    device: Option<ImageMetadata>,
    host_received: u32,
    device_received: u32,
}

impl FirmwareUpdate {
    pub const MAX_IMAGE_SIZE: u32 = 0x18_0000;

    pub const fn new() -> Self {
        Self {
            phase: FirmwareUpdatePhase::Idle,
            target: UpdateTarget::Host,
            bundle_id: [0; 16],
            host: None,
            device: None,
            host_received: 0,
            device_received: 0,
        }
    }

    pub const fn phase(&self) -> FirmwareUpdatePhase {
        self.phase
    }

    pub const fn received(&self, node: FirmwareNode) -> u32 {
        match node {
            FirmwareNode::Host => self.host_received,
            FirmwareNode::Device => self.device_received,
        }
    }

    pub fn begin(
        &mut self,
        target: UpdateTarget,
        bundle_id: [u8; 16],
        host: Option<ImageMetadata>,
        device: Option<ImageMetadata>,
    ) -> Result<(), FirmwareUpdateError> {
        if !matches!(
            self.phase,
            FirmwareUpdatePhase::Idle
                | FirmwareUpdatePhase::Committed
                | FirmwareUpdatePhase::RolledBack
        ) {
            return Err(FirmwareUpdateError::InvalidPhase);
        }
        let required_host = matches!(target, UpdateTarget::Host | UpdateTarget::Both);
        let required_device = matches!(target, UpdateTarget::Device | UpdateTarget::Both);
        if required_host != host.is_some() || required_device != device.is_some() {
            return Err(FirmwareUpdateError::MissingNode);
        }
        if host.is_some_and(|image| image.size > Self::MAX_IMAGE_SIZE)
            || device.is_some_and(|image| image.size > Self::MAX_IMAGE_SIZE)
        {
            return Err(FirmwareUpdateError::ImageTooLarge);
        }
        self.phase = FirmwareUpdatePhase::Staging;
        self.target = target;
        self.bundle_id = bundle_id;
        self.host = host;
        self.device = device;
        self.host_received = 0;
        self.device_received = 0;
        Ok(())
    }

    pub fn accept_chunk(
        &mut self,
        node: FirmwareNode,
        offset: u32,
        length: u32,
    ) -> Result<(), FirmwareUpdateError> {
        if self.phase != FirmwareUpdatePhase::Staging {
            return Err(FirmwareUpdateError::InvalidPhase);
        }
        let (metadata, received) = match node {
            FirmwareNode::Host => (self.host, &mut self.host_received),
            FirmwareNode::Device => (self.device, &mut self.device_received),
        };
        let metadata = metadata.ok_or(FirmwareUpdateError::MissingNode)?;
        if offset != *received {
            return Err(FirmwareUpdateError::WrongOffset);
        }
        *received = received
            .checked_add(length)
            .filter(|total| *total <= metadata.size)
            .ok_or(FirmwareUpdateError::ImageTooLarge)?;
        Ok(())
    }

    pub fn verify(
        &mut self,
        host_hash: Option<[u8; 32]>,
        device_hash: Option<[u8; 32]>,
    ) -> Result<(), FirmwareUpdateError> {
        if self.phase != FirmwareUpdatePhase::Staging {
            return Err(FirmwareUpdateError::InvalidPhase);
        }
        if self.host.is_some_and(|image| {
            self.host_received != image.size || host_hash != Some(image.sha256)
        }) || self.device.is_some_and(|image| {
            self.device_received != image.size || device_hash != Some(image.sha256)
        }) {
            return Err(FirmwareUpdateError::HashMismatch);
        }
        self.phase = FirmwareUpdatePhase::Ready;
        Ok(())
    }

    pub fn request_reboot(&mut self) -> Result<(), FirmwareUpdateError> {
        if self.phase != FirmwareUpdatePhase::Ready {
            return Err(FirmwareUpdateError::InvalidPhase);
        }
        self.phase = FirmwareUpdatePhase::RebootPending;
        Ok(())
    }

    pub fn start_health_check(&mut self, bundle_id: [u8; 16]) -> Result<(), FirmwareUpdateError> {
        if self.phase != FirmwareUpdatePhase::RebootPending {
            return Err(FirmwareUpdateError::InvalidPhase);
        }
        if bundle_id != self.bundle_id {
            self.phase = FirmwareUpdatePhase::RolledBack;
            return Err(FirmwareUpdateError::BundleMismatch);
        }
        self.phase = FirmwareUpdatePhase::HealthCheck;
        Ok(())
    }

    pub fn finish_health_check(&mut self, healthy: bool) -> Result<(), FirmwareUpdateError> {
        if self.phase != FirmwareUpdatePhase::HealthCheck {
            return Err(FirmwareUpdateError::InvalidPhase);
        }
        self.phase = if healthy {
            FirmwareUpdatePhase::Committed
        } else {
            FirmwareUpdatePhase::RolledBack
        };
        Ok(())
    }
}

impl Default for FirmwareUpdate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(node: FirmwareNode, byte: u8) -> ImageMetadata {
        ImageMetadata {
            node,
            size: 16,
            sha256: [byte; 32],
            version: [1, 2, 3],
        }
    }

    #[test]
    fn dual_update_commits_only_after_both_images_and_shared_health_check() {
        let mut update = FirmwareUpdate::new();
        let bundle = [7; 16];
        update
            .begin(
                UpdateTarget::Both,
                bundle,
                Some(image(FirmwareNode::Host, 1)),
                Some(image(FirmwareNode::Device, 2)),
            )
            .unwrap();
        update.accept_chunk(FirmwareNode::Host, 0, 16).unwrap();
        update.accept_chunk(FirmwareNode::Device, 0, 16).unwrap();
        update.verify(Some([1; 32]), Some([2; 32])).unwrap();
        update.request_reboot().unwrap();
        update.start_health_check(bundle).unwrap();
        update.finish_health_check(true).unwrap();
        assert_eq!(update.phase(), FirmwareUpdatePhase::Committed);
    }

    #[test]
    fn interrupted_transfer_reports_resume_offset_and_rejects_gaps() {
        let mut update = FirmwareUpdate::new();
        update
            .begin(
                UpdateTarget::Host,
                [1; 16],
                Some(image(FirmwareNode::Host, 1)),
                None,
            )
            .unwrap();
        update.accept_chunk(FirmwareNode::Host, 0, 8).unwrap();
        assert_eq!(update.received(FirmwareNode::Host), 8);
        assert_eq!(
            update.accept_chunk(FirmwareNode::Host, 12, 4),
            Err(FirmwareUpdateError::WrongOffset)
        );
    }

    #[test]
    fn mismatched_dual_bundle_rolls_back_transaction() {
        let mut update = FirmwareUpdate::new();
        update
            .begin(
                UpdateTarget::Host,
                [1; 16],
                Some(image(FirmwareNode::Host, 1)),
                None,
            )
            .unwrap();
        update.accept_chunk(FirmwareNode::Host, 0, 16).unwrap();
        update.verify(Some([1; 32]), None).unwrap();
        update.request_reboot().unwrap();
        assert_eq!(
            update.start_health_check([2; 16]),
            Err(FirmwareUpdateError::BundleMismatch)
        );
        assert_eq!(update.phase(), FirmwareUpdatePhase::RolledBack);
    }
}
