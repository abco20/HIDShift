#[cfg(feature = "storage")]
use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType, read_partition_table,
};
#[cfg(feature = "storage")]
use esp_storage::FlashStorage;
#[cfg(feature = "storage")]
use hidshift::storage::{NorFlashStorageBackend, STORAGE_FLASH_LEN, StorageFlashLayout};
use hidshift::storage::{STORAGE_IMAGE_LEN, StorageError, StorageSlotBackend, StorageSlotIndex};

#[cfg(feature = "storage")]
pub const STORAGE_PARTITION_LABEL: &str = "bridge";

pub enum FirmwareStorageBackend {
    #[cfg(feature = "storage")]
    Flash(NorFlashStorageBackend<FlashStorage<'static>>),
    Memory(InMemoryStorageBackend),
}

impl StorageSlotBackend for FirmwareStorageBackend {
    fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
        match self {
            #[cfg(feature = "storage")]
            Self::Flash(backend) => backend.slot(index),
            Self::Memory(backend) => backend.slot(index),
        }
    }

    fn write_slot(
        &mut self,
        index: StorageSlotIndex,
        image: [u8; STORAGE_IMAGE_LEN],
    ) -> Result<(), StorageError> {
        match self {
            #[cfg(feature = "storage")]
            Self::Flash(backend) => backend.write_slot(index, image),
            Self::Memory(backend) => backend.write_slot(index, image),
        }
    }
}

#[cfg(feature = "storage")]
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
                "firmware: storage backend flash unavailable {:?}; using in-memory",
                error
            );
            FirmwareStorageBackend::Memory(InMemoryStorageBackend::new())
        }
    }
}

#[cfg(not(feature = "storage"))]
pub fn new_storage_backend() -> FirmwareStorageBackend {
    FirmwareStorageBackend::Memory(InMemoryStorageBackend::new())
}

#[cfg(feature = "storage")]
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

    if partition.len() < STORAGE_FLASH_LEN as u32 {
        return Err(FirmwareStorageInitError::PartitionTooSmall {
            len: partition.len(),
        });
    }

    NorFlashStorageBackend::new(flash, StorageFlashLayout::new(partition.offset()))
        .map_err(FirmwareStorageInitError::Storage)
}

#[cfg(feature = "storage")]
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
