use crate::ids::HostId;

pub const MAX_HOST_NAME_LEN: usize = 32;
pub const STORED_HOSTS_MAX: usize = 4;
pub const STORAGE_MAGIC: [u8; 4] = *b"E32B";
pub const STORAGE_SCHEMA_VERSION: u16 = 3;
pub const STORAGE_IMAGE_LEN: usize = 512;
pub const STORAGE_HEADER_LEN: usize = 16;
pub const STORAGE_BODY_PREFIX_LEN: usize = 8;
pub const STORED_HOST_RECORD_LEN: usize = 88;
pub const STORED_BOND_LEN: usize = 48;
pub const STORAGE_FLASH_SLOT_SIZE: usize = 4096;
pub const STORAGE_FLASH_SLOT_COUNT: usize = 2;
pub const STORAGE_FLASH_LEN: usize = STORAGE_FLASH_SLOT_SIZE * STORAGE_FLASH_SLOT_COUNT;
pub const STORAGE_FLASH_RECORDS_PER_SLOT: usize = STORAGE_FLASH_SLOT_SIZE / STORAGE_IMAGE_LEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredHostProfile {
    pub host_id: HostId,
    pub bonded: bool,
    pub keyboard_cccd_enabled: bool,
    pub mouse_cccd_enabled: bool,
    pub consumer_cccd_enabled: bool,
    pub keyboard_output_cccd_enabled: bool,
    pub name: FixedName,
    pub bond: Option<StoredBond>,
}

impl StoredHostProfile {
    pub const fn empty() -> Self {
        Self {
            host_id: HostId(0),
            bonded: false,
            keyboard_cccd_enabled: false,
            mouse_cccd_enabled: false,
            consumer_cccd_enabled: false,
            keyboard_output_cccd_enabled: false,
            name: FixedName::empty(),
            bond: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredBond {
    pub peer_address: [u8; 6],
    pub peer_irk: Option<[u8; 16]>,
    pub ltk: [u8; 16],
    pub is_bonded: bool,
    pub security_level: StoredSecurityLevel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum StoredSecurityLevel {
    NoEncryption = 0,
    Encrypted = 1,
    EncryptedAuthenticated = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixedName {
    len: u8,
    bytes: [u8; MAX_HOST_NAME_LEN],
}

impl FixedName {
    pub const fn empty() -> Self {
        Self {
            len: 0,
            bytes: [0; MAX_HOST_NAME_LEN],
        }
    }

    pub fn from_ascii(name: &str) -> Option<Self> {
        let bytes = name.as_bytes();
        if bytes.len() > MAX_HOST_NAME_LEN || !bytes.is_ascii() {
            return None;
        }

        let mut fixed = Self::empty();
        fixed.len = bytes.len() as u8;
        fixed.bytes[..bytes.len()].copy_from_slice(bytes);
        Some(fixed)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageState {
    pub generation: u32,
    pub last_active_host: Option<HostId>,
    hosts: heapless::Vec<StoredHostProfile, STORED_HOSTS_MAX>,
}

impl StorageState {
    pub const fn new(generation: u32) -> Self {
        Self {
            generation,
            last_active_host: None,
            hosts: heapless::Vec::new(),
        }
    }

    pub fn push_host(&mut self, host: StoredHostProfile) -> Result<(), StorageError> {
        self.hosts
            .push(host)
            .map_err(|_| StorageError::HostCapacity)
    }

    pub fn hosts(&self) -> &[StoredHostProfile] {
        &self.hosts
    }

    pub fn validate(&self) -> Result<(), StorageError> {
        let mut seen = [false; STORED_HOSTS_MAX];

        for host in self.hosts.iter().copied() {
            let Some(index) = host.host_id.0.checked_sub(1).map(|index| index as usize) else {
                return Err(StorageError::InvalidHostId);
            };
            if index >= STORED_HOSTS_MAX {
                return Err(StorageError::InvalidHostId);
            }
            if seen[index] {
                return Err(StorageError::DuplicateHostId);
            }
            seen[index] = true;
        }

        if let Some(active_host) = self.last_active_host
            && !self.hosts.iter().any(|host| host.host_id == active_host)
        {
            return Err(StorageError::ActiveHostMissing);
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageError {
    HostCapacity,
    InvalidHostId,
    DuplicateHostId,
    ActiveHostMissing,
    ImageTooShort,
    InvalidMagic,
    UnsupportedVersion,
    InvalidLength,
    CrcMismatch,
    HostCountTooLarge,
    InvalidName,
    FlashLayout,
    FlashRead,
    FlashErase,
    FlashWrite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageHeader {
    pub version: u16,
    pub body_len: u16,
    pub generation: u32,
    pub crc32: u32,
}

pub trait StorageSlotBackend {
    fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN];
    fn write_slot(
        &mut self,
        index: StorageSlotIndex,
        image: [u8; STORAGE_IMAGE_LEN],
    ) -> Result<(), StorageError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageWriteResult {
    pub index: StorageSlotIndex,
    pub state: StorageState,
    pub image: [u8; STORAGE_IMAGE_LEN],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageDebouncer {
    delay_ms: u64,
    deadline_ms: Option<u64>,
    pending: Option<StorageState>,
}

impl StorageDebouncer {
    pub const fn new(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            deadline_ms: None,
            pending: None,
        }
    }

    pub fn stage(&mut self, state: StorageState, now_ms: u64) {
        self.pending = Some(state);
        self.deadline_ms = Some(now_ms.saturating_add(self.delay_ms));
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn deadline_ms(&self) -> Option<u64> {
        self.deadline_ms
    }

    pub fn remaining_ms(&self, now_ms: u64) -> Option<u64> {
        self.deadline_ms
            .map(|deadline| deadline.saturating_sub(now_ms))
    }

    pub fn take_due(&mut self, now_ms: u64) -> Option<StorageState> {
        let deadline = self.deadline_ms?;
        if now_ms < deadline {
            return None;
        }
        self.deadline_ms = None;
        self.pending.take()
    }

    pub fn flush(&mut self) -> Option<StorageState> {
        self.deadline_ms = None;
        self.pending.take()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoragePersistence {
    normal_delay_ms: u64,
    lazy_delay_ms: u64,
    pending_state: Option<StorageState>,
    pending_priority: Option<StoragePersistPriority>,
    deadline_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum StoragePersistPriority {
    Lazy,
    Normal,
    Critical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageTaskPolicy {
    pub active_ble_retry_ms: u64,
    pub critical_force_quiesce_ms: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageTaskAction {
    AwaitCommand,
    WaitForDeadline { delay_ms: u64 },
    DeferForActiveBle { delay_ms: u64 },
    QuiesceAndPersist { forced: bool },
}

impl StoragePersistence {
    pub const fn new(normal_delay_ms: u64, lazy_delay_ms: u64) -> Self {
        Self {
            normal_delay_ms,
            lazy_delay_ms,
            pending_state: None,
            pending_priority: None,
            deadline_ms: None,
        }
    }

    pub fn stage(&mut self, state: StorageState, priority: StoragePersistPriority, now_ms: u64) {
        let merged_priority = self
            .pending_priority
            .map(|current| current.max(priority))
            .unwrap_or(priority);
        let new_deadline = now_ms.saturating_add(self.delay_for(merged_priority));

        self.pending_state = Some(state);
        self.pending_priority = Some(merged_priority);
        self.deadline_ms = Some(match self.deadline_ms {
            Some(current_deadline) if merged_priority != priority => {
                current_deadline.min(new_deadline)
            }
            _ => new_deadline,
        });
    }

    pub fn stage_quiesce_snapshot(&mut self, state: StorageState, now_ms: u64) {
        self.stage(state, StoragePersistPriority::Critical, now_ms);
    }

    pub fn is_pending(&self) -> bool {
        self.pending_state.is_some()
    }

    pub fn remaining_ms(&self, now_ms: u64) -> Option<u64> {
        self.deadline_ms
            .map(|deadline| deadline.saturating_sub(now_ms))
    }

    pub fn persist_due<B: StorageSlotBackend>(
        &mut self,
        backend: &mut B,
        now_ms: u64,
    ) -> Result<Option<StorageWriteResult>, StorageError> {
        let Some(deadline_ms) = self.deadline_ms else {
            return Ok(None);
        };
        if now_ms < deadline_ms {
            return Ok(None);
        }
        let Some(state) = self.pending_state.clone() else {
            return Ok(None);
        };
        let result = persist_storage_state(backend, &state)?;
        self.clear_pending();
        Ok(Some(result))
    }

    pub fn flush<B: StorageSlotBackend>(
        &mut self,
        backend: &mut B,
    ) -> Result<Option<StorageWriteResult>, StorageError> {
        let Some(state) = self.pending_state.clone() else {
            return Ok(None);
        };
        let result = persist_storage_state(backend, &state)?;
        self.clear_pending();
        Ok(Some(result))
    }

    pub fn pending_priority(&self) -> Option<StoragePersistPriority> {
        self.pending_priority
    }

    pub fn overdue_ms(&self, now_ms: u64) -> Option<u64> {
        let deadline_ms = self.deadline_ms?;
        Some(now_ms.saturating_sub(deadline_ms))
    }

    fn take_pending(&mut self) -> Option<StorageState> {
        self.deadline_ms = None;
        self.pending_priority = None;
        self.pending_state.take()
    }

    fn clear_pending(&mut self) {
        let _ = self.take_pending();
    }

    const fn delay_for(&self, priority: StoragePersistPriority) -> u64 {
        match priority {
            StoragePersistPriority::Critical => 0,
            StoragePersistPriority::Normal => self.normal_delay_ms,
            StoragePersistPriority::Lazy => self.lazy_delay_ms,
        }
    }
}

impl StorageTaskPolicy {
    pub fn evaluate(
        &self,
        persistence: &StoragePersistence,
        now_ms: u64,
        active_ble_connections: usize,
    ) -> StorageTaskAction {
        if !persistence.is_pending() {
            return StorageTaskAction::AwaitCommand;
        }

        match persistence.remaining_ms(now_ms) {
            Some(delay_ms) if delay_ms > 0 => {
                return StorageTaskAction::WaitForDeadline { delay_ms };
            }
            _ => {}
        }

        if active_ble_connections == 0 {
            return StorageTaskAction::QuiesceAndPersist { forced: false };
        }

        if persistence.pending_priority() == Some(StoragePersistPriority::Critical)
            && persistence.overdue_ms(now_ms).unwrap_or(0) >= self.critical_force_quiesce_ms
        {
            return StorageTaskAction::QuiesceAndPersist { forced: true };
        }

        StorageTaskAction::DeferForActiveBle {
            delay_ms: self.active_ble_retry_ms,
        }
    }
}

pub fn encode_storage_image(state: &StorageState) -> Result<[u8; STORAGE_IMAGE_LEN], StorageError> {
    state.validate()?;
    let mut image = [0u8; STORAGE_IMAGE_LEN];
    image[0..4].copy_from_slice(&STORAGE_MAGIC);
    write_u16(&mut image[4..6], STORAGE_SCHEMA_VERSION);
    let body_len = storage_body_len(state.hosts.len());
    write_u16(&mut image[6..8], body_len as u16);
    write_u32(&mut image[8..12], state.generation);
    image[16] = state.hosts.len() as u8;
    image[17] = state.last_active_host.map_or(0xff, |host| host.0);

    for (index, host) in state.hosts.iter().copied().enumerate() {
        let offset = STORAGE_HEADER_LEN + STORAGE_BODY_PREFIX_LEN + index * STORED_HOST_RECORD_LEN;
        encode_host_record(host, &mut image[offset..offset + STORED_HOST_RECORD_LEN])?;
    }

    let crc32 = crc32_without_header_crc(&image[..STORAGE_HEADER_LEN + body_len]);
    write_u32(&mut image[12..16], crc32);
    Ok(image)
}

pub fn decode_storage_image(image: &[u8]) -> Result<StorageState, StorageError> {
    if image.len() < STORAGE_IMAGE_LEN {
        return Err(StorageError::ImageTooShort);
    }
    if image[0..4] != STORAGE_MAGIC {
        return Err(StorageError::InvalidMagic);
    }

    let version = read_u16(&image[4..6]);
    if version != STORAGE_SCHEMA_VERSION {
        return Err(StorageError::UnsupportedVersion);
    }

    let body_len = read_u16(&image[6..8]) as usize;
    if !body_len_is_valid(body_len) {
        return Err(StorageError::InvalidLength);
    }

    let total_len = STORAGE_HEADER_LEN + body_len;
    let expected_crc32 = read_u32(&image[12..16]);
    let actual_crc32 = crc32_without_header_crc(&image[..total_len]);
    if expected_crc32 != actual_crc32 {
        return Err(StorageError::CrcMismatch);
    }

    let host_count = image[16] as usize;
    if host_count > STORED_HOSTS_MAX {
        return Err(StorageError::HostCountTooLarge);
    }
    if storage_body_len(host_count) != body_len {
        return Err(StorageError::InvalidLength);
    }

    let mut state = StorageState::new(read_u32(&image[8..12]));
    state.last_active_host = match image[17] {
        0xff => None,
        id => Some(HostId(id)),
    };

    for index in 0..host_count {
        let offset = STORAGE_HEADER_LEN + STORAGE_BODY_PREFIX_LEN + index * STORED_HOST_RECORD_LEN;
        state.push_host(decode_host_record(
            &image[offset..offset + STORED_HOST_RECORD_LEN],
        )?)?;
    }

    state.validate()?;

    Ok(state)
}

pub fn select_newest_valid_storage_image<'a>(
    slot_a: &'a [u8],
    slot_b: &'a [u8],
) -> Option<StorageSlot<'a>> {
    let decoded_a = decode_storage_image(slot_a).ok();
    let decoded_b = decode_storage_image(slot_b).ok();

    match (decoded_a, decoded_b) {
        (Some(a), Some(b)) => {
            if generation_is_newer_or_equal(a.generation, b.generation) {
                Some(StorageSlot {
                    index: StorageSlotIndex::A,
                    image: slot_a,
                    state: a,
                })
            } else {
                Some(StorageSlot {
                    index: StorageSlotIndex::B,
                    image: slot_b,
                    state: b,
                })
            }
        }
        (Some(state), None) => Some(StorageSlot {
            index: StorageSlotIndex::A,
            image: slot_a,
            state,
        }),
        (None, Some(state)) => Some(StorageSlot {
            index: StorageSlotIndex::B,
            image: slot_b,
            state,
        }),
        (None, None) => None,
    }
}

pub fn restore_latest_storage_state<B: StorageSlotBackend>(backend: &B) -> Option<StorageState> {
    select_newest_valid_storage_image(
        backend.slot(StorageSlotIndex::A),
        backend.slot(StorageSlotIndex::B),
    )
    .map(|slot| slot.state)
}

pub fn persist_storage_state<B: StorageSlotBackend>(
    backend: &mut B,
    state: &StorageState,
) -> Result<StorageWriteResult, StorageError> {
    state.validate()?;
    let target = match select_newest_valid_storage_image(
        backend.slot(StorageSlotIndex::A),
        backend.slot(StorageSlotIndex::B),
    ) {
        Some(slot) => slot.index.other(),
        None => StorageSlotIndex::A,
    };

    let image = encode_storage_image(state)?;
    backend.write_slot(target, image)?;

    Ok(StorageWriteResult {
        index: target,
        state: state.clone(),
        image,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageSlot<'a> {
    pub index: StorageSlotIndex,
    pub image: &'a [u8],
    pub state: StorageState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StorageSlotIndex {
    A,
    B,
}

impl StorageSlotIndex {
    pub const fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StorageFlashLayout {
    pub base_offset: u32,
    pub slot_size: u32,
}

impl StorageFlashLayout {
    pub const fn new(base_offset: u32) -> Self {
        Self {
            base_offset,
            slot_size: STORAGE_FLASH_SLOT_SIZE as u32,
        }
    }

    pub const fn len(&self) -> u32 {
        self.slot_size * STORAGE_FLASH_SLOT_COUNT as u32
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn records_per_slot(&self) -> u32 {
        self.slot_size / STORAGE_IMAGE_LEN as u32
    }

    pub const fn slot_offset(&self, index: StorageSlotIndex) -> u32 {
        self.base_offset
            + match index {
                StorageSlotIndex::A => 0,
                StorageSlotIndex::B => self.slot_size,
            }
    }

    pub const fn record_offset(&self, index: StorageSlotIndex, record_index: u32) -> u32 {
        self.slot_offset(index) + record_index * STORAGE_IMAGE_LEN as u32
    }

    pub fn validate_for_flash<F>(&self, flash: &F) -> Result<(), StorageError>
    where
        F: embedded_storage::nor_flash::NorFlash,
    {
        let capacity = flash.capacity() as u32;
        if self.slot_size < STORAGE_IMAGE_LEN as u32
            || self.base_offset.checked_add(self.len()).is_none()
            || self.base_offset + self.len() > capacity
            || !is_aligned(self.base_offset as usize, F::ERASE_SIZE)
            || !is_aligned(self.slot_size as usize, F::ERASE_SIZE)
            || !self.slot_size.is_multiple_of(STORAGE_IMAGE_LEN as u32)
            || !is_aligned(self.base_offset as usize, F::WRITE_SIZE)
            || !is_aligned(STORAGE_IMAGE_LEN, F::WRITE_SIZE)
            || !is_aligned(self.base_offset as usize, F::READ_SIZE)
            || !is_aligned(STORAGE_IMAGE_LEN, F::READ_SIZE)
        {
            return Err(StorageError::FlashLayout);
        }

        Ok(())
    }
}

pub struct NorFlashStorageBackend<F> {
    flash: F,
    layout: StorageFlashLayout,
    slots: [[u8; STORAGE_IMAGE_LEN]; STORAGE_FLASH_SLOT_COUNT],
    next_record: [u32; STORAGE_FLASH_SLOT_COUNT],
}

impl<F> NorFlashStorageBackend<F>
where
    F: embedded_storage::nor_flash::NorFlash,
{
    pub fn new(mut flash: F, layout: StorageFlashLayout) -> Result<Self, StorageError> {
        layout.validate_for_flash(&flash)?;

        let mut slots = [[0xffu8; STORAGE_IMAGE_LEN]; STORAGE_FLASH_SLOT_COUNT];
        let mut next_record = [0u32; STORAGE_FLASH_SLOT_COUNT];
        scan_flash_slot(
            &mut flash,
            &layout,
            StorageSlotIndex::A,
            &mut slots[0],
            &mut next_record[0],
        )?;
        scan_flash_slot(
            &mut flash,
            &layout,
            StorageSlotIndex::B,
            &mut slots[1],
            &mut next_record[1],
        )?;

        Ok(Self {
            flash,
            layout,
            slots,
            next_record,
        })
    }

    pub fn release(self) -> F {
        self.flash
    }
}

impl<F> StorageSlotBackend for NorFlashStorageBackend<F>
where
    F: embedded_storage::nor_flash::NorFlash,
{
    fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
        &self.slots[storage_slot_number(index)]
    }

    fn write_slot(
        &mut self,
        index: StorageSlotIndex,
        image: [u8; STORAGE_IMAGE_LEN],
    ) -> Result<(), StorageError> {
        let slot_number = storage_slot_number(index);
        let mut record_index = self.next_record[slot_number];
        let records_per_slot = self.layout.records_per_slot();
        let slot_offset = self.layout.slot_offset(index);
        if record_index >= records_per_slot {
            self.flash
                .erase(slot_offset, slot_offset + self.layout.slot_size)
                .map_err(|_| StorageError::FlashErase)?;
            record_index = 0;
        }
        let offset = slot_offset + record_index * STORAGE_IMAGE_LEN as u32;
        self.flash
            .write(offset, &image)
            .map_err(|_| StorageError::FlashWrite)?;
        self.slots[slot_number] = image;
        self.next_record[slot_number] = record_index + 1;
        Ok(())
    }
}

const fn storage_slot_number(index: StorageSlotIndex) -> usize {
    match index {
        StorageSlotIndex::A => 0,
        StorageSlotIndex::B => 1,
    }
}

fn scan_flash_slot<F>(
    flash: &mut F,
    layout: &StorageFlashLayout,
    index: StorageSlotIndex,
    latest_image: &mut [u8; STORAGE_IMAGE_LEN],
    next_record: &mut u32,
) -> Result<(), StorageError>
where
    F: embedded_storage::nor_flash::NorFlash,
{
    let mut image = [0xffu8; STORAGE_IMAGE_LEN];
    let records_per_slot = layout.records_per_slot();
    *next_record = 0;

    for record_index in 0..records_per_slot {
        let offset = layout.record_offset(index, record_index);
        flash
            .read(offset, &mut image)
            .map_err(|_| StorageError::FlashRead)?;

        if image_is_erased(&image) {
            *next_record = record_index;
            return Ok(());
        }

        if decode_storage_image(&image).is_ok() {
            *latest_image = image;
            *next_record = record_index + 1;
        } else {
            *next_record = records_per_slot;
            return Ok(());
        }
    }

    Ok(())
}

const fn image_is_erased(image: &[u8; STORAGE_IMAGE_LEN]) -> bool {
    let mut index = 0;
    while index < STORAGE_IMAGE_LEN {
        if image[index] != 0xff {
            return false;
        }
        index += 1;
    }
    true
}

fn encode_host_record(host: StoredHostProfile, out: &mut [u8]) -> Result<(), StorageError> {
    out[0] = host.host_id.0;
    out[1] = bool_byte(host.bonded);
    out[2] = bool_byte(host.keyboard_cccd_enabled);
    out[3] = bool_byte(host.mouse_cccd_enabled);
    out[4] = bool_byte(host.consumer_cccd_enabled);
    out[5] = bool_byte(host.keyboard_output_cccd_enabled);
    out[6] = host.name.len;
    out[7] = 0;
    out[8..8 + MAX_HOST_NAME_LEN].copy_from_slice(&host.name.bytes);
    encode_bond(host.bond, &mut out[40..40 + STORED_BOND_LEN])?;
    Ok(())
}

fn decode_host_record(record: &[u8]) -> Result<StoredHostProfile, StorageError> {
    let name_len = record[6] as usize;
    if name_len > MAX_HOST_NAME_LEN {
        return Err(StorageError::InvalidName);
    }

    let mut name = FixedName::empty();
    name.len = name_len as u8;
    name.bytes
        .copy_from_slice(&record[8..8 + MAX_HOST_NAME_LEN]);

    Ok(StoredHostProfile {
        host_id: HostId(record[0]),
        bonded: record[1] != 0,
        keyboard_cccd_enabled: record[2] != 0,
        mouse_cccd_enabled: record[3] != 0,
        consumer_cccd_enabled: record[4] != 0,
        keyboard_output_cccd_enabled: record[5] != 0,
        name,
        bond: decode_bond(&record[40..40 + STORED_BOND_LEN])?,
    })
}

fn encode_bond(bond: Option<StoredBond>, out: &mut [u8]) -> Result<(), StorageError> {
    out.fill(0);
    let Some(bond) = bond else {
        return Ok(());
    };
    out[0] = 1;
    out[1..7].copy_from_slice(&bond.peer_address);
    out[7] = bool_byte(bond.peer_irk.is_some());
    if let Some(irk) = bond.peer_irk {
        out[8..24].copy_from_slice(&irk);
    }
    out[24..40].copy_from_slice(&bond.ltk);
    out[40] = bool_byte(bond.is_bonded);
    out[41] = bond.security_level as u8;
    Ok(())
}

fn decode_bond(record: &[u8]) -> Result<Option<StoredBond>, StorageError> {
    if record.first().copied().unwrap_or(0) == 0 {
        return Ok(None);
    }

    let security_level = match record[41] {
        0 => StoredSecurityLevel::NoEncryption,
        1 => StoredSecurityLevel::Encrypted,
        2 => StoredSecurityLevel::EncryptedAuthenticated,
        _ => return Err(StorageError::InvalidLength),
    };

    let mut peer_address = [0u8; 6];
    peer_address.copy_from_slice(&record[1..7]);
    let peer_irk = if record[7] == 0 {
        None
    } else {
        let mut irk = [0u8; 16];
        irk.copy_from_slice(&record[8..24]);
        Some(irk)
    };
    let mut ltk = [0u8; 16];
    ltk.copy_from_slice(&record[24..40]);

    Ok(Some(StoredBond {
        peer_address,
        peer_irk,
        ltk,
        is_bonded: record[40] != 0,
        security_level,
    }))
}

const fn storage_body_len(host_count: usize) -> usize {
    STORAGE_BODY_PREFIX_LEN + host_count * STORED_HOST_RECORD_LEN
}

const fn body_len_is_valid(body_len: usize) -> bool {
    body_len >= STORAGE_BODY_PREFIX_LEN
        && body_len <= storage_body_len(STORED_HOSTS_MAX)
        && STORAGE_HEADER_LEN + body_len <= STORAGE_IMAGE_LEN
}

fn crc32_without_header_crc(image: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for (index, byte) in image.iter().copied().enumerate() {
        if (12..16).contains(&index) {
            continue;
        }
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

const fn bool_byte(value: bool) -> u8 {
    if value { 1 } else { 0 }
}

const fn is_aligned(value: usize, align: usize) -> bool {
    align != 0 && value.is_multiple_of(align)
}

fn write_u16(out: &mut [u8], value: u16) {
    out.copy_from_slice(&value.to_le_bytes());
}

fn write_u32(out: &mut [u8], value: u32) {
    out.copy_from_slice(&value.to_le_bytes());
}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn generation_is_newer_or_equal(left: u32, right: u32) -> bool {
    left.wrapping_sub(right) < 0x8000_0000
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::convert::Infallible;
    use embedded_storage::nor_flash::{
        ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash, check_erase, check_read, check_write,
    };

    #[derive(Clone)]
    struct TestBackend {
        slot_a: [u8; STORAGE_IMAGE_LEN],
        slot_b: [u8; STORAGE_IMAGE_LEN],
    }

    impl TestBackend {
        fn empty() -> Self {
            Self {
                slot_a: [0; STORAGE_IMAGE_LEN],
                slot_b: [0; STORAGE_IMAGE_LEN],
            }
        }
    }

    impl StorageSlotBackend for TestBackend {
        fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
            match index {
                StorageSlotIndex::A => &self.slot_a,
                StorageSlotIndex::B => &self.slot_b,
            }
        }

        fn write_slot(
            &mut self,
            index: StorageSlotIndex,
            image: [u8; STORAGE_IMAGE_LEN],
        ) -> Result<(), StorageError> {
            match index {
                StorageSlotIndex::A => self.slot_a = image,
                StorageSlotIndex::B => self.slot_b = image,
            }
            Ok(())
        }
    }

    #[test]
    fn fixed_name_rejects_names_that_do_not_fit() {
        let too_long = "012345678901234567890123456789012";

        assert!(FixedName::from_ascii(too_long).is_none());
    }

    #[test]
    fn storage_image_round_trips_profiles_cccd_and_last_active_host() {
        let mut state = StorageState::new(42);
        state.last_active_host = Some(HostId(1));
        state
            .push_host(StoredHostProfile {
                host_id: HostId(1),
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: false,
                consumer_cccd_enabled: true,
                keyboard_output_cccd_enabled: true,
                name: FixedName::from_ascii("laptop").unwrap(),
                bond: None,
            })
            .unwrap();

        let image = encode_storage_image(&state).unwrap();
        let decoded = decode_storage_image(&image).unwrap();

        assert_eq!(decoded, state);
        assert_eq!(&image[0..4], &STORAGE_MAGIC);
    }

    #[test]
    fn storage_image_round_trips_bond_payload() {
        let mut state = StorageState::new(7);
        state
            .push_host(StoredHostProfile {
                host_id: HostId(1),
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: true,
                consumer_cccd_enabled: false,
                keyboard_output_cccd_enabled: false,
                name: FixedName::from_ascii("desktop").unwrap(),
                bond: Some(StoredBond {
                    peer_address: [1, 2, 3, 4, 5, 6],
                    peer_irk: Some([0x11; 16]),
                    ltk: [0x22; 16],
                    is_bonded: true,
                    security_level: StoredSecurityLevel::EncryptedAuthenticated,
                }),
            })
            .unwrap();

        let image = encode_storage_image(&state).unwrap();
        let decoded = decode_storage_image(&image).unwrap();

        assert_eq!(decoded, state);
    }

    #[test]
    fn storage_image_rejects_bad_crc32() {
        let state = StorageState::new(1);
        let mut image = encode_storage_image(&state).unwrap();
        image[17] ^= 0x55;

        assert_eq!(decode_storage_image(&image), Err(StorageError::CrcMismatch));
    }

    #[test]
    fn storage_image_rejects_unsupported_version() {
        let state = StorageState::new(1);
        let mut image = encode_storage_image(&state).unwrap();
        image[4] = 0xff;

        assert_eq!(
            decode_storage_image(&image),
            Err(StorageError::UnsupportedVersion)
        );
    }

    #[test]
    fn storage_slot_selection_chooses_newest_valid_generation() {
        let old = encode_storage_image(&StorageState::new(1)).unwrap();
        let new = encode_storage_image(&StorageState::new(2)).unwrap();

        let selected = select_newest_valid_storage_image(&old, &new).unwrap();

        assert_eq!(selected.index, StorageSlotIndex::B);
        assert_eq!(selected.state.generation, 2);
    }

    #[test]
    fn storage_slot_selection_ignores_invalid_newer_slot() {
        let old = encode_storage_image(&StorageState::new(1)).unwrap();
        let mut new = encode_storage_image(&StorageState::new(2)).unwrap();
        new[17] ^= 0xaa;

        let selected = select_newest_valid_storage_image(&old, &new).unwrap();

        assert_eq!(selected.index, StorageSlotIndex::A);
        assert_eq!(selected.state.generation, 1);
    }

    #[test]
    fn storage_state_enforces_host_capacity() {
        let mut state = StorageState::new(1);
        for index in 0..STORED_HOSTS_MAX {
            let mut host = StoredHostProfile::empty();
            host.host_id = HostId((index + 1) as u8);
            state.push_host(host).unwrap();
        }

        assert_eq!(
            state.push_host(StoredHostProfile::empty()),
            Err(StorageError::HostCapacity)
        );
    }

    #[test]
    fn storage_state_rejects_duplicate_host_id() {
        let mut state = StorageState::new(1);
        let mut first = StoredHostProfile::empty();
        first.host_id = HostId(1);
        let mut second = StoredHostProfile::empty();
        second.host_id = HostId(1);
        state.push_host(first).unwrap();
        state.push_host(second).unwrap();

        assert_eq!(state.validate(), Err(StorageError::DuplicateHostId));
        assert_eq!(
            encode_storage_image(&state),
            Err(StorageError::DuplicateHostId)
        );
    }

    #[test]
    fn storage_state_rejects_active_host_that_is_not_saved() {
        let mut state = StorageState::new(1);
        let mut host = StoredHostProfile::empty();
        host.host_id = HostId(1);
        state.push_host(host).unwrap();
        state.last_active_host = Some(HostId(2));

        assert_eq!(state.validate(), Err(StorageError::ActiveHostMissing));
        assert_eq!(
            persist_storage_state(&mut TestBackend::empty(), &state),
            Err(StorageError::ActiveHostMissing)
        );
    }

    #[test]
    fn storage_state_rejects_invalid_zero_host_id() {
        let mut state = StorageState::new(1);
        state.push_host(StoredHostProfile::empty()).unwrap();

        assert_eq!(state.validate(), Err(StorageError::InvalidHostId));
    }

    #[test]
    fn decode_rejects_duplicate_host_id_records() {
        let mut state = StorageState::new(2);
        let mut first = StoredHostProfile::empty();
        first.host_id = HostId(1);
        state.push_host(first).unwrap();
        let mut image = encode_storage_image(&state).unwrap();

        let duplicate = first;
        let offset = STORAGE_HEADER_LEN + STORAGE_BODY_PREFIX_LEN + STORED_HOST_RECORD_LEN;
        encode_host_record(
            duplicate,
            &mut image[offset..offset + STORED_HOST_RECORD_LEN],
        )
        .unwrap();
        image[16] = 2;
        write_u16(&mut image[6..8], storage_body_len(2) as u16);
        let crc32 = crc32_without_header_crc(&image[..STORAGE_HEADER_LEN + storage_body_len(2)]);
        write_u32(&mut image[12..16], crc32);

        assert_eq!(
            decode_storage_image(&image),
            Err(StorageError::DuplicateHostId)
        );
    }

    #[test]
    fn storage_image_rejects_invalid_body_length() {
        let state = StorageState::new(1);
        let mut image = encode_storage_image(&state).unwrap();
        write_u16(&mut image[6..8], 7);

        assert_eq!(
            decode_storage_image(&image),
            Err(StorageError::InvalidLength)
        );
    }

    #[test]
    fn storage_image_rejects_host_count_body_length_mismatch() {
        let mut state = StorageState::new(1);
        let mut host = StoredHostProfile::empty();
        host.host_id = HostId(1);
        state.push_host(host).unwrap();
        let mut image = encode_storage_image(&state).unwrap();
        write_u16(&mut image[6..8], storage_body_len(0) as u16);
        let crc32 = crc32_without_header_crc(&image[..STORAGE_HEADER_LEN + storage_body_len(0)]);
        write_u32(&mut image[12..16], crc32);

        assert_eq!(
            decode_storage_image(&image),
            Err(StorageError::InvalidLength)
        );
    }

    #[test]
    fn storage_slot_selection_ignores_partially_written_newer_slot() {
        let old = encode_storage_image(&StorageState::new(1)).unwrap();
        let mut new = encode_storage_image(&StorageState::new(2)).unwrap();
        let body_len = read_u16(&new[6..8]) as usize;
        new[STORAGE_HEADER_LEN + body_len - 1] ^= 0x5a;

        let selected = select_newest_valid_storage_image(&old, &new).unwrap();

        assert_eq!(selected.index, StorageSlotIndex::A);
        assert_eq!(selected.state.generation, 1);
    }

    #[test]
    fn persist_storage_state_writes_slot_a_first_when_both_slots_are_invalid() {
        let mut backend = TestBackend::empty();
        let state = StorageState::new(7);

        let result = persist_storage_state(&mut backend, &state).unwrap();

        assert_eq!(result.index, StorageSlotIndex::A);
        assert_eq!(
            decode_storage_image(backend.slot(StorageSlotIndex::A)).unwrap(),
            state
        );
        assert_eq!(
            decode_storage_image(backend.slot(StorageSlotIndex::B)),
            Err(StorageError::InvalidMagic)
        );
    }

    #[test]
    fn persist_storage_state_alternates_away_from_newest_valid_slot() {
        let mut backend = TestBackend::empty();
        persist_storage_state(&mut backend, &StorageState::new(1)).unwrap();

        let result = persist_storage_state(&mut backend, &StorageState::new(2)).unwrap();

        assert_eq!(result.index, StorageSlotIndex::B);
        assert_eq!(
            decode_storage_image(backend.slot(StorageSlotIndex::A))
                .unwrap()
                .generation,
            1
        );
        assert_eq!(
            decode_storage_image(backend.slot(StorageSlotIndex::B))
                .unwrap()
                .generation,
            2
        );
    }

    #[test]
    fn restore_latest_storage_state_returns_newest_valid_generation() {
        let mut backend = TestBackend::empty();
        persist_storage_state(&mut backend, &StorageState::new(11)).unwrap();
        persist_storage_state(&mut backend, &StorageState::new(12)).unwrap();

        let restored = restore_latest_storage_state(&backend).unwrap();

        assert_eq!(restored.generation, 12);
    }

    #[test]
    fn storage_debouncer_does_not_flush_before_deadline() {
        let mut debouncer = StorageDebouncer::new(250);
        debouncer.stage(StorageState::new(1), 1_000);

        assert!(debouncer.is_pending());
        assert_eq!(debouncer.deadline_ms(), Some(1_250));
        assert_eq!(debouncer.remaining_ms(1_100), Some(150));
        assert_eq!(debouncer.take_due(1_249), None);

        let due = debouncer.take_due(1_250).unwrap();
        assert_eq!(due.generation, 1);
        assert!(!debouncer.is_pending());
    }

    #[test]
    fn storage_debouncer_coalesces_updates_to_latest_state() {
        let mut debouncer = StorageDebouncer::new(500);
        debouncer.stage(StorageState::new(1), 1_000);
        debouncer.stage(StorageState::new(2), 1_100);
        debouncer.stage(StorageState::new(3), 1_200);

        assert_eq!(debouncer.deadline_ms(), Some(1_700));
        assert_eq!(debouncer.take_due(1_699), None);

        let due = debouncer.take_due(1_700).unwrap();
        assert_eq!(due.generation, 3);
        assert_eq!(debouncer.take_due(2_000), None);
    }

    #[test]
    fn storage_debouncer_flush_takes_pending_state_immediately() {
        let mut debouncer = StorageDebouncer::new(10_000);
        debouncer.stage(StorageState::new(9), 1);

        let flushed = debouncer.flush().unwrap();

        assert_eq!(flushed.generation, 9);
        assert!(!debouncer.is_pending());
        assert_eq!(debouncer.deadline_ms(), None);
    }

    #[test]
    fn storage_persistence_writes_only_after_debounce_deadline() {
        let mut backend = CountingBackend::empty();
        let mut persistence = StoragePersistence::new(100, 1_000);
        persistence.stage(StorageState::new(1), StoragePersistPriority::Normal, 1_000);

        assert_eq!(persistence.persist_due(&mut backend, 1_099), Ok(None));
        assert_eq!(backend.write_count, 0);

        let result = persistence
            .persist_due(&mut backend, 1_100)
            .unwrap()
            .unwrap();

        assert_eq!(result.index, StorageSlotIndex::A);
        assert_eq!(result.state.generation, 1);
        assert_eq!(backend.write_count, 1);
    }

    #[test]
    fn storage_persistence_coalesces_multiple_snapshots_before_write() {
        let mut backend = CountingBackend::empty();
        let mut persistence = StoragePersistence::new(100, 1_000);
        persistence.stage(StorageState::new(1), StoragePersistPriority::Normal, 1_000);
        persistence.stage(StorageState::new(2), StoragePersistPriority::Normal, 1_050);
        persistence.stage(StorageState::new(3), StoragePersistPriority::Normal, 1_075);

        assert_eq!(persistence.persist_due(&mut backend, 1_174), Ok(None));
        let result = persistence
            .persist_due(&mut backend, 1_175)
            .unwrap()
            .unwrap();

        assert_eq!(result.state.generation, 3);
        assert_eq!(backend.write_count, 1);
        assert_eq!(
            restore_latest_storage_state(&backend).unwrap().generation,
            3
        );
    }

    #[test]
    fn quiesce_snapshot_replaces_pending_state_and_is_due_immediately() {
        let mut persistence = StoragePersistence::new(1_000, 5_000);
        persistence.stage(StorageState::new(1), StoragePersistPriority::Normal, 0);

        persistence.stage_quiesce_snapshot(StorageState::new(2), 100);

        assert_eq!(
            persistence.pending_priority(),
            Some(StoragePersistPriority::Critical)
        );
        let mut backend = CountingBackend::empty();
        let result = persistence.persist_due(&mut backend, 100).unwrap().unwrap();
        assert_eq!(result.state.generation, 2);
    }

    #[test]
    fn storage_persistence_flush_writes_pending_snapshot_immediately() {
        let mut backend = CountingBackend::empty();
        let mut persistence = StoragePersistence::new(10_000, 60_000);
        persistence.stage(StorageState::new(7), StoragePersistPriority::Lazy, 1);

        let result = persistence.flush(&mut backend).unwrap().unwrap();

        assert_eq!(result.state.generation, 7);
        assert_eq!(backend.write_count, 1);
        assert_eq!(persistence.flush(&mut backend), Ok(None));
    }

    #[test]
    fn critical_persistence_is_due_immediately() {
        let mut backend = CountingBackend::empty();
        let mut persistence = StoragePersistence::new(100, 1_000);
        persistence.stage(
            StorageState::new(5),
            StoragePersistPriority::Critical,
            1_000,
        );

        let result = persistence
            .persist_due(&mut backend, 1_000)
            .unwrap()
            .unwrap();

        assert_eq!(result.state.generation, 5);
        assert_eq!(backend.write_count, 1);
    }

    #[test]
    fn newer_lazy_snapshot_keeps_older_critical_urgency() {
        let mut backend = CountingBackend::empty();
        let mut persistence = StoragePersistence::new(100, 1_000);
        persistence.stage(
            StorageState::new(5),
            StoragePersistPriority::Critical,
            1_000,
        );
        persistence.stage(StorageState::new(6), StoragePersistPriority::Lazy, 1_010);

        assert_eq!(
            persistence.pending_priority(),
            Some(StoragePersistPriority::Critical)
        );

        let result = persistence
            .persist_due(&mut backend, 1_010)
            .unwrap()
            .unwrap();

        assert_eq!(result.state.generation, 6);
        assert_eq!(backend.write_count, 1);
    }

    #[test]
    fn flash_write_failure_keeps_critical_pending() {
        let mut backend = FailOnceBackend::new();
        let mut persistence = StoragePersistence::new(100, 1_000);
        persistence.stage(
            StorageState::new(9),
            StoragePersistPriority::Critical,
            1_000,
        );

        let first = persistence.persist_due(&mut backend, 1_000);
        assert_eq!(first, Err(StorageError::FlashWrite));
        assert!(persistence.is_pending());
        assert_eq!(
            persistence.pending_priority(),
            Some(StoragePersistPriority::Critical)
        );

        let retried = persistence
            .persist_due(&mut backend, 1_000)
            .unwrap()
            .unwrap();
        assert_eq!(retried.state.generation, 9);
        assert_eq!(backend.write_count, 2);
        assert!(!persistence.is_pending());
    }

    #[test]
    fn storage_task_policy_waits_for_command_without_pending_snapshot() {
        let persistence = StoragePersistence::new(100, 1_000);
        let policy = StorageTaskPolicy {
            active_ble_retry_ms: 250,
            critical_force_quiesce_ms: 2_000,
        };

        assert_eq!(
            policy.evaluate(&persistence, 1_000, 0),
            StorageTaskAction::AwaitCommand
        );
    }

    #[test]
    fn storage_task_policy_waits_for_pending_deadline() {
        let mut persistence = StoragePersistence::new(100, 1_000);
        let policy = StorageTaskPolicy {
            active_ble_retry_ms: 250,
            critical_force_quiesce_ms: 2_000,
        };
        persistence.stage(StorageState::new(1), StoragePersistPriority::Normal, 1_000);

        assert_eq!(
            policy.evaluate(&persistence, 1_050, 0),
            StorageTaskAction::WaitForDeadline { delay_ms: 50 }
        );
    }

    #[test]
    fn storage_task_policy_quiesces_due_snapshot_when_ble_is_idle() {
        let mut persistence = StoragePersistence::new(100, 1_000);
        let policy = StorageTaskPolicy {
            active_ble_retry_ms: 250,
            critical_force_quiesce_ms: 2_000,
        };
        persistence.stage(StorageState::new(1), StoragePersistPriority::Normal, 1_000);

        assert_eq!(
            policy.evaluate(&persistence, 1_100, 0),
            StorageTaskAction::QuiesceAndPersist { forced: false }
        );
    }

    #[test]
    fn storage_task_policy_defers_noncritical_due_snapshot_while_ble_is_active() {
        let mut persistence = StoragePersistence::new(100, 1_000);
        let policy = StorageTaskPolicy {
            active_ble_retry_ms: 250,
            critical_force_quiesce_ms: 2_000,
        };
        persistence.stage(StorageState::new(1), StoragePersistPriority::Normal, 1_000);

        assert_eq!(
            policy.evaluate(&persistence, 1_100, 1),
            StorageTaskAction::DeferForActiveBle { delay_ms: 250 }
        );
    }

    #[test]
    fn storage_task_policy_forces_quiesce_for_overdue_critical_snapshot() {
        let mut persistence = StoragePersistence::new(100, 1_000);
        let policy = StorageTaskPolicy {
            active_ble_retry_ms: 250,
            critical_force_quiesce_ms: 2_000,
        };
        persistence.stage(
            StorageState::new(1),
            StoragePersistPriority::Critical,
            1_000,
        );

        assert_eq!(
            policy.evaluate(&persistence, 3_000, 1),
            StorageTaskAction::QuiesceAndPersist { forced: true }
        );
    }

    #[test]
    fn flash_layout_places_two_erase_aligned_storage_slots() {
        let layout = StorageFlashLayout::new(0x10000);

        assert_eq!(layout.len(), STORAGE_FLASH_LEN as u32);
        assert_eq!(
            layout.records_per_slot(),
            STORAGE_FLASH_RECORDS_PER_SLOT as u32
        );
        assert_eq!(layout.slot_offset(StorageSlotIndex::A), 0x10000);
        assert_eq!(
            layout.slot_offset(StorageSlotIndex::B),
            0x10000 + STORAGE_FLASH_SLOT_SIZE as u32
        );
    }

    #[test]
    fn nor_flash_backend_restores_latest_slot_and_persists_to_other_slot() {
        let layout = StorageFlashLayout::new(0);
        let mut flash = TestNorFlash::<{ STORAGE_FLASH_LEN }>::new();
        let old = encode_storage_image(&StorageState::new(1)).unwrap();
        let new = encode_storage_image(&StorageState::new(2)).unwrap();
        flash.seed(layout.slot_offset(StorageSlotIndex::A), &old);
        flash.seed(layout.slot_offset(StorageSlotIndex::B), &new);

        let mut backend = NorFlashStorageBackend::new(flash, layout).unwrap();
        assert_eq!(
            restore_latest_storage_state(&backend).unwrap().generation,
            2
        );

        let result = persist_storage_state(&mut backend, &StorageState::new(3)).unwrap();
        assert_eq!(result.index, StorageSlotIndex::A);
        assert_eq!(
            decode_storage_image(backend.slot(StorageSlotIndex::A))
                .unwrap()
                .generation,
            3
        );

        let flash = backend.release();
        assert_eq!(flash.erase_calls, 0);
        assert_eq!(flash.write_calls, 1);
        assert_eq!(flash.last_erased, None);
        assert_eq!(
            flash.last_written,
            Some((STORAGE_IMAGE_LEN as u32, STORAGE_IMAGE_LEN))
        );
    }

    #[test]
    fn nor_flash_backend_restores_latest_record_within_each_sector() {
        let layout = StorageFlashLayout::new(0);
        let mut flash = TestNorFlash::<{ STORAGE_FLASH_LEN }>::new();
        let a1 = encode_storage_image(&StorageState::new(1)).unwrap();
        let a3 = encode_storage_image(&StorageState::new(3)).unwrap();
        let b2 = encode_storage_image(&StorageState::new(2)).unwrap();
        flash.seed(layout.record_offset(StorageSlotIndex::A, 0), &a1);
        flash.seed(layout.record_offset(StorageSlotIndex::A, 1), &a3);
        flash.seed(layout.record_offset(StorageSlotIndex::B, 0), &b2);

        let backend = NorFlashStorageBackend::new(flash, layout).unwrap();
        assert_eq!(
            decode_storage_image(backend.slot(StorageSlotIndex::A))
                .unwrap()
                .generation,
            3
        );
        assert_eq!(
            restore_latest_storage_state(&backend).unwrap().generation,
            3
        );
    }

    #[test]
    fn nor_flash_backend_erases_sector_only_when_journal_slot_is_full() {
        let layout = StorageFlashLayout::new(0);
        let mut flash = TestNorFlash::<{ STORAGE_FLASH_LEN }>::new();
        for record_index in 0..STORAGE_FLASH_RECORDS_PER_SLOT as u32 {
            let generation = record_index + 1;
            let image = encode_storage_image(&StorageState::new(generation)).unwrap();
            flash.seed(
                layout.record_offset(StorageSlotIndex::A, record_index),
                &image,
            );
        }
        let b_latest = encode_storage_image(&StorageState::new(100)).unwrap();
        flash.seed(layout.record_offset(StorageSlotIndex::B, 0), &b_latest);

        let mut backend = NorFlashStorageBackend::new(flash, layout).unwrap();
        let result = persist_storage_state(&mut backend, &StorageState::new(101)).unwrap();
        assert_eq!(result.index, StorageSlotIndex::A);
        assert_eq!(
            decode_storage_image(backend.slot(StorageSlotIndex::A))
                .unwrap()
                .generation,
            101
        );

        let flash = backend.release();
        assert_eq!(flash.erase_calls, 1);
        assert_eq!(flash.write_calls, 1);
        assert_eq!(flash.last_erased, Some((0, STORAGE_FLASH_SLOT_SIZE as u32)));
        assert_eq!(flash.last_written, Some((0, STORAGE_IMAGE_LEN)));
    }

    #[test]
    fn nor_flash_backend_rejects_layout_that_does_not_fit_flash() {
        let layout = StorageFlashLayout::new(0);
        let flash = TestNorFlash::<{ STORAGE_FLASH_LEN - 1 }>::new();

        assert!(matches!(
            NorFlashStorageBackend::new(flash, layout),
            Err(StorageError::FlashLayout)
        ));
    }

    #[derive(Clone)]
    struct TestNorFlash<const SIZE: usize> {
        bytes: [u8; SIZE],
        erase_calls: usize,
        write_calls: usize,
        last_erased: Option<(u32, u32)>,
        last_written: Option<(u32, usize)>,
    }

    impl<const SIZE: usize> TestNorFlash<SIZE> {
        fn new() -> Self {
            Self {
                bytes: [0xff; SIZE],
                erase_calls: 0,
                write_calls: 0,
                last_erased: None,
                last_written: None,
            }
        }

        fn seed(&mut self, offset: u32, bytes: &[u8]) {
            let offset = offset as usize;
            self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
        }
    }

    impl<const SIZE: usize> ErrorType for TestNorFlash<SIZE> {
        type Error = Infallible;
    }

    impl<const SIZE: usize> ReadNorFlash for TestNorFlash<SIZE> {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            check_read(self, offset, bytes.len()).map_err(infallible_flash_error)?;
            let offset = offset as usize;
            bytes.copy_from_slice(&self.bytes[offset..offset + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            SIZE
        }
    }

    impl<const SIZE: usize> NorFlash for TestNorFlash<SIZE> {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = STORAGE_FLASH_SLOT_SIZE;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            check_erase(self, from, to).map_err(infallible_flash_error)?;
            self.erase_calls += 1;
            self.last_erased = Some((from, to));
            self.bytes[from as usize..to as usize].fill(0xff);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            check_write(self, offset, bytes.len()).map_err(infallible_flash_error)?;
            self.write_calls += 1;
            self.last_written = Some((offset, bytes.len()));
            let offset = offset as usize;
            self.bytes[offset..offset + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }
    }

    fn infallible_flash_error(_: NorFlashErrorKind) -> Infallible {
        unreachable!("test flash layout should be valid")
    }

    #[derive(Clone)]
    struct CountingBackend {
        inner: TestBackend,
        write_count: usize,
    }

    impl CountingBackend {
        fn empty() -> Self {
            Self {
                inner: TestBackend::empty(),
                write_count: 0,
            }
        }
    }

    impl StorageSlotBackend for CountingBackend {
        fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
            self.inner.slot(index)
        }

        fn write_slot(
            &mut self,
            index: StorageSlotIndex,
            image: [u8; STORAGE_IMAGE_LEN],
        ) -> Result<(), StorageError> {
            self.write_count += 1;
            self.inner.write_slot(index, image)
        }
    }

    #[derive(Clone)]
    struct FailOnceBackend {
        inner: TestBackend,
        fail_next_write: bool,
        write_count: usize,
    }

    impl FailOnceBackend {
        fn new() -> Self {
            Self {
                inner: TestBackend::empty(),
                fail_next_write: true,
                write_count: 0,
            }
        }
    }

    impl StorageSlotBackend for FailOnceBackend {
        fn slot(&self, index: StorageSlotIndex) -> &[u8; STORAGE_IMAGE_LEN] {
            self.inner.slot(index)
        }

        fn write_slot(
            &mut self,
            index: StorageSlotIndex,
            image: [u8; STORAGE_IMAGE_LEN],
        ) -> Result<(), StorageError> {
            self.write_count += 1;
            if self.fail_next_write {
                self.fail_next_write = false;
                return Err(StorageError::FlashWrite);
            }
            self.inner.write_slot(index, image)
        }
    }
}
