use embedded_storage::nor_flash::{NorFlash, ReadNorFlash};
use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType, read_partition_table,
};
use esp_storage::FlashStorage;
use hidshift::mirror::{
    MIRROR_PROFILE_PARTITION_LEN, ProfileStore, ProfileStoreBackend, ProfileStoreError,
};

const PROFILE_PARTITION_LABEL: &str = "mirror";

pub type DeviceProfileStore = ProfileStore<DeviceProfileBackend>;

pub fn open(
    flash: esp_hal::peripherals::FLASH<'static>,
) -> Result<DeviceProfileStore, ProfileStoreInitError> {
    let mut flash = FlashStorage::new(flash);
    let mut partition_table = [0; PARTITION_TABLE_MAX_LEN];
    let table = read_partition_table(&mut flash, &mut partition_table)
        .map_err(|_| ProfileStoreInitError::PartitionTable)?;
    let partition = table
        .iter()
        .find(|partition| {
            partition.label_as_str() == PROFILE_PARTITION_LABEL
                && partition.partition_type()
                    == PartitionType::Data(DataPartitionSubType::Undefined)
        })
        .ok_or(ProfileStoreInitError::PartitionMissing)?;
    if partition.len() < MIRROR_PROFILE_PARTITION_LEN as u32 {
        return Err(ProfileStoreInitError::PartitionTooSmall);
    }
    Ok(ProfileStore::new(DeviceProfileBackend {
        flash,
        base_offset: partition.offset(),
    }))
}

pub struct DeviceProfileBackend {
    flash: FlashStorage<'static>,
    base_offset: u32,
}

impl ProfileStoreBackend for DeviceProfileBackend {
    type Error = ProfileFlashError;

    fn read(&mut self, offset: usize, out: &mut [u8]) -> Result<(), Self::Error> {
        self.flash
            .read(self.absolute(offset)?, out)
            .map_err(|_| ProfileFlashError::Read)
    }

    fn erase(&mut self, offset: usize, length: usize) -> Result<(), Self::Error> {
        let from = self.absolute(offset)?;
        let length = u32::try_from(length).map_err(|_| ProfileFlashError::Bounds)?;
        let to = from.checked_add(length).ok_or(ProfileFlashError::Bounds)?;
        self.flash
            .erase(from, to)
            .map_err(|_| ProfileFlashError::Erase)
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Self::Error> {
        self.flash
            .write(self.absolute(offset)?, data)
            .map_err(|_| ProfileFlashError::Write)
    }
}

impl DeviceProfileBackend {
    fn absolute(&self, offset: usize) -> Result<u32, ProfileFlashError> {
        let offset = u32::try_from(offset).map_err(|_| ProfileFlashError::Bounds)?;
        self.base_offset
            .checked_add(offset)
            .ok_or(ProfileFlashError::Bounds)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileStoreInitError {
    PartitionTable,
    PartitionMissing,
    PartitionTooSmall,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProfileFlashError {
    Bounds,
    Read,
    Erase,
    Write,
}

pub fn storage_error_result<E>(
    error: ProfileStoreError<E>,
    transfer_id: u32,
    profile_hash: u32,
) -> hidshift::interchip::ProfileResult {
    let _ = error;
    hidshift::interchip::ProfileResult {
        transfer_id,
        profile_hash,
        status: hidshift::interchip::ProfileResultStatus::StorageError,
        reject_reason: hidshift::mirror::MirrorRejectReason::StorageFailure as u8,
        detail: 0,
    }
}
