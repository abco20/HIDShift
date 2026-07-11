use crate::ids::HostId;
use crate::storage::{StorageState, StoredBond};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleInputGate<const HOSTS: usize> {
    blocked: [bool; HOSTS],
}

impl<const HOSTS: usize> BleInputGate<HOSTS> {
    pub const fn new() -> Self {
        Self {
            blocked: [false; HOSTS],
        }
    }

    pub fn block(&mut self, host_id: HostId) {
        if let Some(index) = host_index::<HOSTS>(host_id) {
            self.blocked[index] = true;
        }
    }

    pub fn activate(&mut self, host_id: HostId) {
        if let Some(index) = host_index::<HOSTS>(host_id) {
            self.blocked[index] = false;
        }
    }

    pub fn should_drop_input(&self, host_id: HostId) -> bool {
        host_index::<HOSTS>(host_id)
            .and_then(|index| self.blocked.get(index))
            .copied()
            .unwrap_or(true)
    }
}

impl<const HOSTS: usize> Default for BleInputGate<HOSTS> {
    fn default() -> Self {
        Self::new()
    }
}

fn host_index<const HOSTS: usize>(host_id: HostId) -> Option<usize> {
    let index = host_id.validated().ok()?.get().checked_sub(1)? as usize;
    (index < HOSTS).then_some(index)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BleConnectionSlotError {
    InvalidSlot,
    InvalidHost,
    NoFreeSlot,
    DuplicateHost,
    DuplicatePeer,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlePeerIdentity {
    pub peer_address: [u8; 6],
    pub peer_irk: Option<[u8; 16]>,
}

impl BlePeerIdentity {
    pub fn matches_bond(self, bond: StoredBond) -> bool {
        match (self.peer_irk, bond.peer_irk) {
            (Some(peer_irk), Some(bond_irk)) if peer_irk == bond_irk => true,
            _ => self.peer_address == bond.peer_address,
        }
    }

    pub fn matches_peer(self, other: Self) -> bool {
        match (self.peer_irk, other.peer_irk) {
            (Some(irk), Some(other_irk)) if irk == other_irk => true,
            _ => self.peer_address == other.peer_address,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleConnectionEntry {
    pub host_id: HostId,
    pub peer_identity: BlePeerIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BleConnectionSlots<const SLOTS: usize> {
    entries: [Option<BleConnectionEntry>; SLOTS],
}

impl<const SLOTS: usize> BleConnectionSlots<SLOTS> {
    pub const fn new() -> Self {
        Self {
            entries: [None; SLOTS],
        }
    }

    pub const fn with_entries(entries: [Option<BleConnectionEntry>; SLOTS]) -> Self {
        Self { entries }
    }

    pub fn connect_first_free(
        &mut self,
        host_id: HostId,
        peer_identity: BlePeerIdentity,
    ) -> Result<BleConnectionSlot, BleConnectionSlotError> {
        host_id
            .validated()
            .map_err(|_| BleConnectionSlotError::InvalidHost)?;
        self.ensure_unique(host_id, peer_identity, None)?;
        for slot in 0..SLOTS {
            if self.entries[slot].is_none() {
                self.entries[slot] = Some(BleConnectionEntry {
                    host_id,
                    peer_identity,
                });
                return Ok(BleConnectionSlot::new(slot, host_id));
            }
        }
        Err(BleConnectionSlotError::NoFreeSlot)
    }

    pub fn set_connected(
        &mut self,
        slot: usize,
        host_id: HostId,
        peer_identity: BlePeerIdentity,
    ) -> Result<BleConnectionSlot, BleConnectionSlotError> {
        self.slot(slot)?;
        host_id
            .validated()
            .map_err(|_| BleConnectionSlotError::InvalidHost)?;
        self.ensure_unique(host_id, peer_identity, Some(slot))?;
        self.entries[slot] = Some(BleConnectionEntry {
            host_id,
            peer_identity,
        });
        Ok(BleConnectionSlot::new(slot, host_id))
    }

    fn ensure_unique(
        &self,
        host_id: HostId,
        peer_identity: BlePeerIdentity,
        replacing_slot: Option<usize>,
    ) -> Result<(), BleConnectionSlotError> {
        for (slot, entry) in self.entries.iter().enumerate() {
            if replacing_slot == Some(slot) {
                continue;
            }
            let Some(entry) = entry else {
                continue;
            };
            if entry.host_id == host_id {
                return Err(BleConnectionSlotError::DuplicateHost);
            }
            if entry.peer_identity.matches_peer(peer_identity) {
                return Err(BleConnectionSlotError::DuplicatePeer);
            }
        }
        Ok(())
    }

    pub fn set_disconnected(
        &mut self,
        slot: usize,
    ) -> Result<Option<BleConnectionSlot>, BleConnectionSlotError> {
        self.slot(slot)?;
        let entry = self.entries[slot].take();
        Ok(entry.map(|entry| BleConnectionSlot::new(slot, entry.host_id)))
    }

    pub fn dispatch_slot_for_host(&self, host_id: HostId) -> Option<BleConnectionSlot> {
        self.entries
            .iter()
            .enumerate()
            .find_map(|(slot, entry)| match entry {
                Some(entry) if entry.host_id == host_id => {
                    Some(BleConnectionSlot::new(slot, entry.host_id))
                }
                _ => None,
            })
    }

    pub fn host_for_slot(&self, slot: usize) -> Option<HostId> {
        self.entries
            .get(slot)
            .and_then(|entry| entry.as_ref().map(|entry| entry.host_id))
    }

    pub fn entry_for_slot(&self, slot: usize) -> Option<BleConnectionEntry> {
        self.entries.get(slot).copied().flatten()
    }

    pub fn should_advertise(&self) -> bool {
        self.entries.iter().any(Option::is_none)
    }

    pub fn is_connected(&self, slot: usize) -> bool {
        self.entries.get(slot).map(Option::is_some).unwrap_or(false)
    }

    pub fn connected_count(&self) -> usize {
        self.entries.iter().flatten().count()
    }

    fn slot(&self, slot: usize) -> Result<(), BleConnectionSlotError> {
        if slot < SLOTS {
            Ok(())
        } else {
            Err(BleConnectionSlotError::InvalidSlot)
        }
    }
}

impl<const SLOTS: usize> Default for BleConnectionSlots<SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BleConnectionSlot {
    index: usize,
    host_id: HostId,
}

impl BleConnectionSlot {
    const fn new(index: usize, host_id: HostId) -> Self {
        Self { index, host_id }
    }

    pub const fn index(self) -> usize {
        self.index
    }

    pub const fn host_id(self) -> HostId {
        self.host_id
    }
}

pub fn resolve_host_id(
    storage: Option<&StorageState>,
    peer_identity: BlePeerIdentity,
    pairing_host: Option<HostId>,
) -> Option<HostId> {
    if let Some(storage) = storage {
        for host in storage.hosts().iter().copied() {
            if let Some(bond) = host.bond
                && peer_identity.matches_bond(bond)
            {
                return Some(host.host_id);
            }
        }
    }

    if pairing_host.is_some() {
        return pairing_host;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{FixedName, StoredHostProfile, StoredSecurityLevel};

    fn peer(index: u8) -> BlePeerIdentity {
        BlePeerIdentity {
            peer_address: [index, 2, 3, 4, 5, 6],
            peer_irk: Some([index; 16]),
        }
    }

    fn stored_host(host_id: u8, identity: BlePeerIdentity) -> StoredHostProfile {
        StoredHostProfile {
            host_id: HostId(host_id),
            bonded: true,
            keyboard_cccd_enabled: false,
            mouse_cccd_enabled: false,
            consumer_cccd_enabled: false,
            keyboard_output_cccd_enabled: false,
            name: FixedName::empty(),
            bond: Some(StoredBond {
                peer_address: identity.peer_address,
                peer_irk: identity.peer_irk,
                ltk: [host_id; 16],
                is_bonded: true,
                security_level: StoredSecurityLevel::Encrypted,
            }),
        }
    }

    #[test]
    fn connections_fill_free_slots_and_stop_advertising_when_full() {
        let mut slots = BleConnectionSlots::<2>::new();

        let first = slots.connect_first_free(HostId(3), peer(3)).unwrap();
        let second = slots.connect_first_free(HostId(1), peer(1)).unwrap();

        assert_eq!(first.index(), 0);
        assert_eq!(first.host_id(), HostId(3));
        assert_eq!(second.index(), 1);
        assert_eq!(second.host_id(), HostId(1));
        assert_eq!(slots.connected_count(), 2);
        assert!(!slots.should_advertise());
        assert_eq!(
            slots.connect_first_free(HostId(2), peer(2)),
            Err(BleConnectionSlotError::NoFreeSlot)
        );
    }

    #[test]
    fn disconnected_slot_is_reused_for_next_connection() {
        let mut slots = BleConnectionSlots::<2>::with_entries([
            Some(BleConnectionEntry {
                host_id: HostId(3),
                peer_identity: peer(3),
            }),
            Some(BleConnectionEntry {
                host_id: HostId(1),
                peer_identity: peer(1),
            }),
        ]);

        assert_eq!(
            slots.set_disconnected(0).unwrap(),
            Some(BleConnectionSlot::new(0, HostId(3)))
        );
        assert!(slots.should_advertise());

        let reconnected = slots.connect_first_free(HostId(2), peer(2)).unwrap();

        assert_eq!(reconnected.index(), 0);
        assert_eq!(reconnected.host_id(), HostId(2));
        assert!(slots.is_connected(0));
        assert!(slots.is_connected(1));
    }

    #[test]
    fn commands_dispatch_only_to_matching_connected_host() {
        let slots = BleConnectionSlots::<2>::with_entries([
            None,
            Some(BleConnectionEntry {
                host_id: HostId(3),
                peer_identity: peer(3),
            }),
        ]);

        assert_eq!(slots.dispatch_slot_for_host(HostId(1)), None);
        assert_eq!(
            slots.dispatch_slot_for_host(HostId(3)),
            Some(BleConnectionSlot::new(1, HostId(3)))
        );
    }

    #[test]
    fn four_host_slots_dispatch_last_connected_host() {
        let mut slots = BleConnectionSlots::<4>::new();

        let slot0 = slots.connect_first_free(HostId(1), peer(1)).unwrap();
        let slot1 = slots.connect_first_free(HostId(2), peer(2)).unwrap();
        let slot2 = slots.connect_first_free(HostId(3), peer(3)).unwrap();
        let slot3 = slots.connect_first_free(HostId(4), peer(4)).unwrap();

        assert_eq!(slot0.index(), 0);
        assert_eq!(slot1.index(), 1);
        assert_eq!(slot2.index(), 2);
        assert_eq!(slot3.index(), 3);
        assert_eq!(slots.connected_count(), 4);
        assert_eq!(
            slots.dispatch_slot_for_host(HostId(4)),
            Some(BleConnectionSlot::new(3, HostId(4)))
        );
        assert!(!slots.should_advertise());
    }

    #[test]
    fn restored_host_identity_does_not_depend_on_connection_order() {
        let mut storage = StorageState::new(1);
        storage.push_host(stored_host(1, peer(1))).unwrap();
        storage.push_host(stored_host(2, peer(2))).unwrap();
        storage.push_host(stored_host(3, peer(3))).unwrap();

        assert_eq!(
            resolve_host_id(Some(&storage), peer(3), None),
            Some(HostId(3))
        );
        assert_eq!(
            resolve_host_id(Some(&storage), peer(1), None),
            Some(HostId(1))
        );
    }

    #[test]
    fn pairing_host_is_used_for_unknown_peer() {
        let mut storage = StorageState::new(1);
        storage.push_host(stored_host(1, peer(1))).unwrap();

        assert_eq!(
            resolve_host_id(Some(&storage), peer(9), Some(HostId(4))),
            Some(HostId(4))
        );
    }

    #[test]
    fn unknown_peer_outside_pairing_does_not_fallback_to_host1() {
        let mut storage = StorageState::new(1);
        storage.push_host(stored_host(1, peer(1))).unwrap();
        storage.push_host(stored_host(3, peer(3))).unwrap();

        assert_eq!(resolve_host_id(Some(&storage), peer(9), None), None);
    }

    #[test]
    fn unknown_peer_inside_pairing_uses_pairing_target_host() {
        let mut storage = StorageState::new(1);
        storage.push_host(stored_host(1, peer(1))).unwrap();

        assert_eq!(
            resolve_host_id(Some(&storage), peer(9), Some(HostId(2))),
            Some(HostId(2))
        );
    }

    #[test]
    fn invalid_slot_is_an_explicit_error_without_mutation() {
        let mut slots = BleConnectionSlots::<2>::with_entries([
            Some(BleConnectionEntry {
                host_id: HostId(2),
                peer_identity: peer(2),
            }),
            None,
        ]);

        assert_eq!(
            slots.set_connected(2, HostId(1), peer(1)),
            Err(BleConnectionSlotError::InvalidSlot)
        );
        assert_eq!(
            slots.set_disconnected(2),
            Err(BleConnectionSlotError::InvalidSlot)
        );
        assert_eq!(
            slots,
            BleConnectionSlots::with_entries([
                Some(BleConnectionEntry {
                    host_id: HostId(2),
                    peer_identity: peer(2),
                }),
                None,
            ])
        );
    }

    #[test]
    fn duplicate_host_and_peer_are_rejected_without_mutation() {
        let existing = BleConnectionEntry {
            host_id: HostId(1),
            peer_identity: peer(1),
        };
        let mut slots = BleConnectionSlots::<2>::with_entries([Some(existing), None]);

        assert_eq!(
            slots.connect_first_free(HostId(1), peer(2)),
            Err(BleConnectionSlotError::DuplicateHost)
        );
        assert_eq!(
            slots.connect_first_free(HostId(2), peer(1)),
            Err(BleConnectionSlotError::DuplicatePeer)
        );
        assert_eq!(
            slots,
            BleConnectionSlots::with_entries([Some(existing), None])
        );
    }

    #[test]
    fn duplicate_peer_irk_is_rejected_even_when_address_rotates() {
        let irk = [7; 16];
        let mut existing_peer = peer(1);
        existing_peer.peer_irk = Some(irk);
        let mut rotated_peer = peer(2);
        rotated_peer.peer_irk = Some(irk);
        let mut slots = BleConnectionSlots::<2>::with_entries([
            Some(BleConnectionEntry {
                host_id: HostId(1),
                peer_identity: existing_peer,
            }),
            None,
        ]);

        assert_eq!(
            slots.connect_first_free(HostId(2), rotated_peer),
            Err(BleConnectionSlotError::DuplicatePeer)
        );
    }

    #[test]
    fn target_release_blocks_stale_input_until_explicit_reactivation() {
        let mut gate = BleInputGate::<4>::new();
        gate.block(HostId(1));
        gate.activate(HostId(2));

        assert!(gate.should_drop_input(HostId(1)));
        assert!(!gate.should_drop_input(HostId(2)));

        gate.activate(HostId(1));
        assert!(!gate.should_drop_input(HostId(1)));
        assert!(gate.should_drop_input(HostId(0)));
    }
}
