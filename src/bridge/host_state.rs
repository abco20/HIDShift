use crate::ids::HostId;
use crate::input::KeyboardLedState;
use crate::reports::ReportKind;
use crate::storage::{FixedName, StorageError, StorageState, StoredBond, StoredHostProfile};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BleHostStateMachine<const HOSTS: usize> {
    active_target: Option<HostId>,
    hosts: [Option<HostRuntimeState>; HOSTS],
}

impl<const HOSTS: usize> BleHostStateMachine<HOSTS> {
    pub const fn new() -> Self {
        Self {
            active_target: None,
            hosts: [None; HOSTS],
        }
    }

    pub const fn active_target(&self) -> Option<HostId> {
        self.active_target
    }

    pub const fn hosts(&self) -> &[Option<HostRuntimeState>; HOSTS] {
        &self.hosts
    }

    pub fn set_active_target(&mut self, host_id: HostId) -> Result<bool, HostStateError> {
        self.upsert_host(host_id)?;
        let changed = self.active_target != Some(host_id);
        self.active_target = Some(host_id);
        Ok(changed)
    }

    pub fn clear_active_target(&mut self) {
        self.active_target = None;
    }

    pub fn on_connected(&mut self, host_id: HostId) -> Result<(), HostStateError> {
        let host = self.upsert_host(host_id)?;
        host.connected = true;
        host.encrypted = false;
        host.session_report_ready = ReportReady::default();
        Ok(())
    }

    pub fn on_disconnected(&mut self, host_id: HostId) {
        if let Some(host) = self.host_mut(host_id) {
            host.connected = false;
            host.encrypted = false;
            host.session_report_ready = ReportReady::default();
        }
    }

    pub fn on_security_changed(
        &mut self,
        host_id: HostId,
        encrypted: bool,
        bonded: bool,
        bond: Option<StoredBond>,
    ) -> Result<bool, HostStateError> {
        let host = self.upsert_host(host_id)?;
        let stored_bonded = bonded || host.bond.is_some();
        let persist = host.bonded != stored_bonded || (bond.is_some() && host.bond != bond);
        host.encrypted = encrypted;
        host.bonded = stored_bonded;
        if let Some(bond) = bond {
            host.bond = Some(bond);
        }
        if host.connected && host.encrypted && host.bonded {
            host.session_report_ready = host.stored_report_ready;
        } else if !host.encrypted {
            host.session_report_ready = ReportReady::default();
        }
        Ok(persist)
    }

    pub fn on_cccd_changed(
        &mut self,
        host_id: HostId,
        report: ReportKind,
        enabled: bool,
    ) -> Result<bool, HostStateError> {
        let host = self.upsert_host(host_id)?;
        let persist = host.stored_report_ready.get(report) != enabled;
        host.stored_report_ready.set(report, enabled);
        host.session_report_ready.set(report, enabled);
        Ok(persist)
    }

    pub fn on_keyboard_led_changed(
        &mut self,
        host_id: HostId,
        leds: KeyboardLedState,
    ) -> Result<(), HostStateError> {
        self.upsert_host(host_id)?.keyboard_leds = Some(leds);
        Ok(())
    }

    pub fn clear_host(&mut self, host_id: HostId) -> bool {
        let Some(index) = self.host_index(host_id) else {
            return false;
        };
        self.hosts[index] = None;
        if self.active_target == Some(host_id) {
            self.active_target = None;
        }
        true
    }

    pub fn restore(&mut self, storage: &StorageState) -> Result<(), HostStateError> {
        self.active_target = storage.last_active_host;
        self.hosts = [None; HOSTS];

        for stored_host in storage.hosts().iter().copied() {
            let host = self.upsert_host(stored_host.host_id)?;
            host.bonded = stored_host.bonded;
            host.connected = false;
            host.encrypted = false;
            host.stored_report_ready = ReportReady {
                keyboard: stored_host.keyboard_cccd_enabled,
                mouse: stored_host.mouse_cccd_enabled,
                consumer: stored_host.consumer_cccd_enabled,
                keyboard_output: stored_host.keyboard_output_cccd_enabled,
            };
            host.session_report_ready = ReportReady::default();
            host.keyboard_leds = None;
            host.name = stored_host.name;
            host.discovered_name = FixedName::empty();
            host.bond = stored_host.bond;
        }

        Ok(())
    }

    pub fn storage_state(&self, generation: u32) -> Result<StorageState, StorageError> {
        let mut state = StorageState::new(generation);
        state.last_active_host = self.active_target;

        for host in self.hosts.iter().flatten().copied() {
            state.push_host(StoredHostProfile {
                host_id: host.host_id,
                bonded: host.bonded,
                keyboard_cccd_enabled: host.stored_report_ready.keyboard,
                mouse_cccd_enabled: host.stored_report_ready.mouse,
                consumer_cccd_enabled: host.stored_report_ready.consumer,
                keyboard_output_cccd_enabled: host.stored_report_ready.keyboard_output,
                name: host.name,
                bond: host.bond,
            })?;
        }

        Ok(state)
    }

    pub fn can_send(&self, host_id: HostId, kind: ReportKind) -> bool {
        self.host(host_id)
            .map(|host| host.can_send(kind))
            .unwrap_or(false)
    }

    pub fn host(&self, host_id: HostId) -> Option<&HostRuntimeState> {
        self.host_index(host_id)
            .and_then(|index| self.hosts[index].as_ref())
    }

    pub fn host_mut(&mut self, host_id: HostId) -> Option<&mut HostRuntimeState> {
        self.host_index(host_id)
            .and_then(|index| self.hosts[index].as_mut())
    }

    pub fn set_name(&mut self, host_id: HostId, name: FixedName) -> Result<bool, HostStateError> {
        let host = self.upsert_host(host_id)?;
        let changed = host.name != name;
        host.name = name;
        Ok(changed)
    }

    pub fn set_discovered_name(
        &mut self,
        host_id: HostId,
        name: FixedName,
    ) -> Result<bool, HostStateError> {
        let host = self.upsert_host(host_id)?;
        let changed = host.discovered_name != name;
        host.discovered_name = name;
        Ok(changed)
    }

    pub fn next_connected_target(&self) -> Option<HostId> {
        self.next_connected_target_after(self.active_target)
    }

    pub fn next_connected_target_after(&self, current: Option<HostId>) -> Option<HostId> {
        let mut first_connected = None;
        let mut return_next = current.is_none();

        for host in self.hosts.iter().flatten().copied() {
            if !host.connected {
                continue;
            }
            if first_connected.is_none() {
                first_connected = Some(host.host_id);
            }
            if return_next {
                return Some(host.host_id);
            }
            if Some(host.host_id) == current {
                return_next = true;
            }
        }

        first_connected
    }

    pub fn pairing_candidate(&self) -> Option<HostId> {
        if let Some(active) = self.active_target
            && self.host(active).is_some_and(|host| !host.has_bond())
        {
            return Some(active);
        }

        for index in 0..HOSTS {
            let host_id = HostId(u8::try_from(index + 1).ok()?);
            match self.host(host_id) {
                Some(host) if host.has_bond() => {}
                _ => return Some(host_id),
            }
        }
        None
    }

    fn upsert_host(&mut self, host_id: HostId) -> Result<&mut HostRuntimeState, HostStateError> {
        let _slot = host_id
            .validated()
            .map_err(|_| HostStateError::InvalidHostId)?;
        if let Some(index) = self.host_index(host_id) {
            return self.hosts[index]
                .as_mut()
                .ok_or(HostStateError::HostCapacity);
        }

        let Some(index) = self.hosts.iter().position(Option::is_none) else {
            return Err(HostStateError::HostCapacity);
        };
        self.hosts[index] = Some(HostRuntimeState {
            host_id,
            ..HostRuntimeState::default()
        });
        self.hosts[index]
            .as_mut()
            .ok_or(HostStateError::HostCapacity)
    }

    fn host_index(&self, host_id: HostId) -> Option<usize> {
        self.hosts
            .iter()
            .position(|host| matches!(host, Some(host) if host.host_id == host_id))
    }
}

impl<const HOSTS: usize> Default for BleHostStateMachine<HOSTS> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostRuntimeState {
    pub host_id: HostId,
    pub connected: bool,
    pub encrypted: bool,
    pub bonded: bool,
    pub stored_report_ready: ReportReady,
    pub session_report_ready: ReportReady,
    pub keyboard_leds: Option<KeyboardLedState>,
    pub name: FixedName,
    pub discovered_name: FixedName,
    pub bond: Option<StoredBond>,
}

impl HostRuntimeState {
    pub const fn can_send(self, kind: ReportKind) -> bool {
        self.connected && self.encrypted && self.session_report_ready.get(kind)
    }

    const fn has_bond(self) -> bool {
        self.bonded || self.bond.is_some()
    }
}

impl Default for HostRuntimeState {
    fn default() -> Self {
        Self {
            host_id: HostId(0),
            connected: false,
            encrypted: false,
            bonded: false,
            stored_report_ready: ReportReady::default(),
            session_report_ready: ReportReady::default(),
            keyboard_leds: None,
            name: FixedName::empty(),
            discovered_name: FixedName::empty(),
            bond: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReportReady {
    pub keyboard: bool,
    pub mouse: bool,
    pub consumer: bool,
    pub keyboard_output: bool,
}

impl ReportReady {
    pub const fn get(self, kind: ReportKind) -> bool {
        match kind {
            ReportKind::Keyboard => self.keyboard,
            ReportKind::Mouse => self.mouse,
            ReportKind::Consumer => self.consumer,
            ReportKind::KeyboardOutput => self.keyboard_output,
        }
    }

    pub fn set(&mut self, kind: ReportKind, enabled: bool) {
        match kind {
            ReportKind::Keyboard => self.keyboard = enabled,
            ReportKind::Mouse => self.mouse = enabled,
            ReportKind::Consumer => self.consumer = enabled,
            ReportKind::KeyboardOutput => self.keyboard_output = enabled,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostStateError {
    InvalidHostId,
    HostCapacity,
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_A: HostId = HostId(1);
    const HOST_B: HostId = HostId(2);

    #[test]
    fn notify_readiness_uses_session_cccd_not_just_stored_profile() {
        let mut hosts = BleHostStateMachine::<2>::new();

        hosts
            .on_cccd_changed(HOST_A, ReportKind::Keyboard, true)
            .unwrap();
        assert!(!hosts.can_send(HOST_A, ReportKind::Keyboard));

        hosts.on_connected(HOST_A).unwrap();
        hosts
            .on_security_changed(HOST_A, true, false, None)
            .unwrap();
        assert!(!hosts.can_send(HOST_A, ReportKind::Keyboard));

        hosts.on_security_changed(HOST_A, true, true, None).unwrap();
        assert!(hosts.can_send(HOST_A, ReportKind::Keyboard));
    }

    #[test]
    fn disconnect_clears_session_but_keeps_persisted_cccd() {
        let mut hosts = BleHostStateMachine::<2>::new();
        hosts.on_connected(HOST_A).unwrap();
        hosts.on_security_changed(HOST_A, true, true, None).unwrap();
        hosts
            .on_cccd_changed(HOST_A, ReportKind::Keyboard, true)
            .unwrap();

        hosts.on_disconnected(HOST_A);

        let host = hosts.host(HOST_A).unwrap();
        assert_eq!(host.session_report_ready, ReportReady::default());
        assert!(host.stored_report_ready.keyboard);
        assert!(!hosts.can_send(HOST_A, ReportKind::Keyboard));

        let snapshot = hosts.storage_state(7).unwrap();
        assert!(snapshot.hosts()[0].keyboard_cccd_enabled);
    }

    #[test]
    fn restore_rehydrates_stored_cccd_and_bond_for_next_encrypted_session() {
        let mut storage = StorageState::new(3);
        storage.last_active_host = Some(HOST_B);
        storage
            .push_host(StoredHostProfile {
                host_id: HOST_B,
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: false,
                consumer_cccd_enabled: true,
                keyboard_output_cccd_enabled: false,
                name: FixedName::empty(),
                bond: None,
            })
            .unwrap();
        let mut hosts = BleHostStateMachine::<2>::new();

        hosts.restore(&storage).unwrap();
        assert_eq!(hosts.active_target(), Some(HOST_B));
        assert!(!hosts.can_send(HOST_B, ReportKind::Keyboard));

        hosts.on_connected(HOST_B).unwrap();
        hosts.on_security_changed(HOST_B, true, true, None).unwrap();

        assert!(hosts.can_send(HOST_B, ReportKind::Keyboard));
        assert!(hosts.can_send(HOST_B, ReportKind::Consumer));
        assert!(!hosts.can_send(HOST_B, ReportKind::Mouse));
    }

    #[test]
    fn security_event_without_bond_payload_keeps_restored_bond() {
        let stored_bond = StoredBond {
            peer_address: [1, 2, 3, 4, 5, 6],
            peer_address_kind: crate::storage::StoredAddressKind::Public,
            peer_irk: None,
            ltk: [0x42; 16],
            is_bonded: true,
            security_level: crate::storage::StoredSecurityLevel::Encrypted,
        };
        let mut storage = StorageState::new(3);
        storage
            .push_host(StoredHostProfile {
                host_id: HOST_A,
                bonded: true,
                keyboard_cccd_enabled: true,
                mouse_cccd_enabled: false,
                consumer_cccd_enabled: false,
                keyboard_output_cccd_enabled: false,
                name: FixedName::empty(),
                bond: Some(stored_bond),
            })
            .unwrap();
        let mut hosts = BleHostStateMachine::<2>::new();

        hosts.restore(&storage).unwrap();
        hosts.on_connected(HOST_A).unwrap();
        let persist = hosts
            .on_security_changed(HOST_A, true, false, None)
            .unwrap();

        assert!(!persist);
        assert!(hosts.can_send(HOST_A, ReportKind::Keyboard));
        let snapshot = hosts.storage_state(4).unwrap();
        assert!(snapshot.hosts()[0].bonded);
        assert_eq!(snapshot.hosts()[0].bond, Some(stored_bond));
    }

    #[test]
    fn next_connected_target_cycles_across_connected_hosts() {
        let mut hosts = BleHostStateMachine::<3>::new();
        hosts.on_connected(HOST_A).unwrap();
        hosts.on_connected(HOST_B).unwrap();
        hosts.set_active_target(HOST_A).unwrap();

        assert_eq!(hosts.next_connected_target(), Some(HOST_B));

        hosts.set_active_target(HOST_B).unwrap();
        assert_eq!(hosts.next_connected_target(), Some(HOST_A));
    }

    #[test]
    fn next_connected_target_skips_disconnected_hosts() {
        let mut hosts = BleHostStateMachine::<3>::new();
        hosts.on_connected(HOST_A).unwrap();
        hosts.on_connected(HOST_B).unwrap();
        hosts.on_disconnected(HOST_A);
        hosts.set_active_target(HOST_A).unwrap();

        assert_eq!(hosts.next_connected_target(), Some(HOST_B));
    }

    #[test]
    fn pairing_candidate_prefers_active_unbonded_host() {
        let mut hosts = BleHostStateMachine::<4>::new();
        hosts.set_active_target(HOST_B).unwrap();

        assert_eq!(hosts.pairing_candidate(), Some(HOST_B));
    }

    #[test]
    fn pairing_candidate_uses_first_free_host_after_bonded_active_host() {
        let mut hosts = BleHostStateMachine::<4>::new();
        hosts.set_active_target(HOST_A).unwrap();
        hosts.on_security_changed(HOST_A, true, true, None).unwrap();

        assert_eq!(hosts.pairing_candidate(), Some(HOST_B));
    }

    #[test]
    fn pairing_candidate_does_not_replace_a_bond_when_all_hosts_are_full() {
        let mut hosts = BleHostStateMachine::<2>::new();
        hosts.set_active_target(HOST_A).unwrap();
        hosts.on_security_changed(HOST_A, true, true, None).unwrap();
        hosts.on_security_changed(HOST_B, true, true, None).unwrap();

        assert_eq!(hosts.pairing_candidate(), None);
    }
}
