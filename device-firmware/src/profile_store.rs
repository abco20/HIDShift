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
        const READ_SIZE: usize = 4;
        let address = self.absolute(offset)?;
        if offset % READ_SIZE != 0 {
            return Err(ProfileFlashError::Bounds);
        }
        let aligned_length = out.len() / READ_SIZE * READ_SIZE;
        if aligned_length != 0 {
            self.flash
                .read(address, &mut out[..aligned_length])
                .map_err(|_| ProfileFlashError::Read)?;
        }
        if aligned_length != out.len() {
            let mut tail = [0; READ_SIZE];
            self.flash
                .read(address + aligned_length as u32, &mut tail)
                .map_err(|_| ProfileFlashError::Read)?;
            let remaining = out.len() - aligned_length;
            out[aligned_length..].copy_from_slice(&tail[..remaining]);
        }
        Ok(())
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
        const WRITE_SIZE: usize = 4;
        let address = self.absolute(offset)?;
        if offset % WRITE_SIZE != 0 {
            return Err(ProfileFlashError::Bounds);
        }
        let aligned_length = data.len() / WRITE_SIZE * WRITE_SIZE;
        if aligned_length != 0 {
            self.flash
                .write(address, &data[..aligned_length])
                .map_err(|_| ProfileFlashError::Write)?;
        }
        let tail = &data[aligned_length..];
        if !tail.is_empty() {
            let mut padded = [0xff; WRITE_SIZE];
            padded[..tail.len()].copy_from_slice(tail);
            self.flash
                .write(address + aligned_length as u32, &padded)
                .map_err(|_| ProfileFlashError::Write)?;
        }
        Ok(())
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

pub fn storage_error_result(
    error: ProfileStoreError<ProfileFlashError>,
    transfer_id: u32,
    profile_hash: u32,
) -> hidshift::interchip::ProfileResult {
    let detail = match error {
        ProfileStoreError::Backend(ProfileFlashError::Bounds) => 1,
        ProfileStoreError::Backend(ProfileFlashError::Read) => 2,
        ProfileStoreError::Backend(ProfileFlashError::Erase) => 3,
        ProfileStoreError::Backend(ProfileFlashError::Write) => 4,
        ProfileStoreError::ImageTooLarge => 5,
        ProfileStoreError::ReadBackMismatch => 6,
    };
    hidshift::interchip::ProfileResult {
        transfer_id,
        profile_hash,
        status: hidshift::interchip::ProfileResultStatus::StorageError,
        reject_reason: hidshift::mirror::MirrorRejectReason::StorageFailure as u8,
        detail,
    }
}
