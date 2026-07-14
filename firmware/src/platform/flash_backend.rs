use esp_bootloader_esp_idf::partitions::{
    DataPartitionSubType, PARTITION_TABLE_MAX_LEN, PartitionType, read_partition_table,
};
use esp_storage::FlashStorage;
use hidshift::espnow_pairing::EspNowPairing;
use hidshift::storage::{
    NorFlashStorageBackend, STORAGE_PARTITION_REQUIRED_LEN, StorageFlashLayout,
};
use hidshift::storage::{STORAGE_IMAGE_LEN, StorageError, StorageSlotBackend, StorageSlotIndex};

pub const STORAGE_PARTITION_LABEL: &str = "bridge";

pub enum FirmwareStorageBackend {
    Flash(NorFlashStorageBackend<FlashStorage<'static>>),
    Memory(InMemoryStorageBackend),
}

impl FirmwareStorageBackend {
    pub fn restored_pairing(&self) -> Option<EspNowPairing> {
        match self {
            Self::Flash(backend) => backend.restored_pairing(),
            Self::Memory(backend) => backend.pairing,
        }
    }

    pub fn write_pairing(&mut self, pairing: EspNowPairing) -> Result<(), StorageError> {
        match self {
            Self::Flash(backend) => backend.write_pairing(pairing),
            Self::Memory(backend) => {
                backend.pairing = Some(pairing);
                Ok(())
            }
        }
    }

    pub fn clear_pairing(&mut self) -> Result<(), StorageError> {
        match self {
            Self::Flash(backend) => backend.clear_pairing(),
            Self::Memory(backend) => {
                backend.pairing = None;
                Ok(())
            }
        }
    }
}

impl StorageSlotBackend for FirmwareStorageBackend {
    fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
        match self {
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
            Self::Flash(backend) => backend.write_slot(index, image),
            Self::Memory(backend) => backend.write_slot(index, image),
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
                "firmware: storage backend flash unavailable {:?}; using in-memory",
                error
            );
            FirmwareStorageBackend::Memory(InMemoryStorageBackend::new())
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

    NorFlashStorageBackend::new_with_pairing(flash, StorageFlashLayout::new(partition.offset()))
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
    pairing: Option<EspNowPairing>,
}

impl InMemoryStorageBackend {
    pub const fn new() -> Self {
        Self {
            slots: [[0; STORAGE_IMAGE_LEN]; 2],
            pairing: None,
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
