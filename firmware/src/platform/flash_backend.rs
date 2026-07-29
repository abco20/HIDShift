use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType, read_partition_table,
};
use esp_storage::FlashStorage;
use hidshift::storage::{
    NorFlashStorageBackend, STORAGE_PARTITION_REQUIRED_LEN, StorageFlashLayout, StorageHealth,
};
use hidshift::storage::{STORAGE_IMAGE_LEN, StorageError, StorageSlotBackend, StorageSlotIndex};

pub const STORAGE_PARTITION_LABEL: &str = "bridge";

pub enum FirmwareStorageBackend {
    Flash(NorFlashStorageBackend<FlashStorage<'static>>),
    #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
    Volatile(InMemoryStorageBackend),
    Unavailable(InMemoryStorageBackend),
}

impl FirmwareStorageBackend {
    pub const fn health(&self) -> StorageHealth {
        match self {
            Self::Flash(_) => StorageHealth::Persistent,
            #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
            Self::Volatile(_) => StorageHealth::VolatileTest,
            Self::Unavailable(_) => StorageHealth::Unavailable,
        }
    }

    pub fn factory_reset(&mut self) -> Result<(), StorageError> {
        match self {
            Self::Flash(backend) => backend.erase_all(),
            #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
            Self::Volatile(backend) => {
                backend.clear();
                Ok(())
            }
            Self::Unavailable(_) => Err(StorageError::Unavailable),
        }
    }
}

impl StorageSlotBackend for FirmwareStorageBackend {
    fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
        match self {
            Self::Flash(backend) => backend.slot(index),
            #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
            Self::Volatile(backend) | Self::Unavailable(backend) => backend.slot(index),
            #[cfg(not(all(feature = "hardware-e2e", feature = "dual-s3-wired")))]
            Self::Unavailable(backend) => backend.slot(index),
        }
    }

    fn write_slot(
        &mut self,
        index: StorageSlotIndex,
        image: [u8; STORAGE_IMAGE_LEN],
    ) -> Result<(), StorageError> {
        match self {
            Self::Flash(backend) => backend.write_slot(index, image),
            #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
            Self::Volatile(backend) => backend.write_slot(index, image),
            Self::Unavailable(_) => Err(StorageError::Unavailable),
        }
    }
}

pub fn new_storage_backend(flash: esp_hal::peripherals::FLASH<'static>) -> FirmwareStorageBackend {
    match new_flash_storage_backend(flash) {
        Ok(backend) => {
            log::info!(
                "firmware: storage backend flash partition={}",
                STORAGE_PARTITION_LABEL
            );
            FirmwareStorageBackend::Flash(backend)
        }
        Err(error) => {
            log::error!(
                "firmware: storage backend flash unavailable {:?}; persistent mutations disabled",
                error
            );
            FirmwareStorageBackend::Unavailable(InMemoryStorageBackend::new())
        }
    }
}

fn new_flash_storage_backend(
    flash: esp_hal::peripherals::FLASH<'static>,
) -> Result<NorFlashStorageBackend<FlashStorage<'static>>, FirmwareStorageInitError> {
    let mut flash = FlashStorage::new(flash).multicore_auto_park();
    let mut partition_table = [0u8; PARTITION_TABLE_MAX_LEN];
    let table = read_partition_table(&mut flash, &mut partition_table)
        .map_err(|_| FirmwareStorageInitError::PartitionTable)?;
    let partition = table
        .iter()
        .find(|partition| {
            partition.label_as_str() == STORAGE_PARTITION_LABEL
                && partition.partition_type()
                    == PartitionType::Data(DataPartitionSubType::Undefined)
        })
        .ok_or(FirmwareStorageInitError::PartitionMissing)?;

    if partition.len() < STORAGE_PARTITION_REQUIRED_LEN as u32 {
        return Err(FirmwareStorageInitError::PartitionTooSmall {
            len: partition.len(),
        });
    }

    NorFlashStorageBackend::new(flash, StorageFlashLayout::new(partition.offset()))
        .map_err(FirmwareStorageInitError::Storage)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FirmwareStorageInitError {
    PartitionTable,
    PartitionMissing,
    PartitionTooSmall { len: u32 },
    Storage(StorageError),
}

pub struct InMemoryStorageBackend {
    slots: [[u8; STORAGE_IMAGE_LEN]; 2],
}

impl InMemoryStorageBackend {
    pub const fn new() -> Self {
        Self {
            slots: [[0; STORAGE_IMAGE_LEN]; 2],
        }
    }

    #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
    fn clear(&mut self) {
        self.slots = [[0; STORAGE_IMAGE_LEN]; 2];
    }
}

impl StorageSlotBackend for InMemoryStorageBackend {
    fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
        let slot = match index {
            StorageSlotIndex::A => 0,
            StorageSlotIndex::B => 1,
        };
        &self.slots[slot]
    }

    fn write_slot(
        &mut self,
        index: StorageSlotIndex,
        image: [u8; STORAGE_IMAGE_LEN],
    ) -> Result<(), StorageError> {
        let slot = match index {
            StorageSlotIndex::A => 0,
            StorageSlotIndex::B => 1,
        };
        self.slots[slot] = image;
        Ok(())
    }
}
