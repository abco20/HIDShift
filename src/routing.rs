use crate::ids::HostId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActiveTargetError {
    CapacityZero,
    OutOfRange,
    HostNotFound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostRouter<const N: usize> {
    active_index: usize,
    hosts: [Option<HostId>; N],
}

impl<const N: usize> HostRouter<N> {
    pub const fn new() -> Self {
        Self {
            active_index: 0,
            hosts: [None; N],
        }
    }

    pub const fn active(&self) -> Option<HostId> {
        if N == 0 {
            return None;
        }

        self.hosts[self.active_index]
    }

    pub const fn active_slot(&self) -> Option<usize> {
        if N == 0 {
            None
        } else {
            Some(self.active_index)
        }
    }

    pub fn host(&self, slot: usize) -> Result<Option<HostId>, ActiveTargetError> {
        if slot >= N {
            return Err(ActiveTargetError::OutOfRange);
        }

        Ok(self.hosts[slot])
    }

    pub fn set_host(&mut self, slot: usize, host: HostId) -> Result<(), ActiveTargetError> {
        if slot >= N {
            return Err(ActiveTargetError::OutOfRange);
        }

        self.hosts[slot] = Some(host);
        Ok(())
    }

    pub fn clear_host(&mut self, slot: usize) -> Result<(), ActiveTargetError> {
        if slot >= N {
            return Err(ActiveTargetError::OutOfRange);
        }

        self.hosts[slot] = None;
        Ok(())
    }

    pub fn set_active_slot(&mut self, slot: usize) -> Result<(), ActiveTargetError> {
        if N == 0 {
            return Err(ActiveTargetError::CapacityZero);
        }
        if slot >= N {
            return Err(ActiveTargetError::OutOfRange);
        }

        self.active_index = slot;
        Ok(())
    }

    pub fn set_active_host(&mut self, host: HostId) -> Result<usize, ActiveTargetError> {
        if N == 0 {
            return Err(ActiveTargetError::CapacityZero);
        }

        let Some(slot) = self.slot_for_host(host) else {
            return Err(ActiveTargetError::HostNotFound);
        };
        self.active_index = slot;
        Ok(slot)
    }

    pub fn slot_for_host(&self, host: HostId) -> Option<usize> {
        self.hosts
            .iter()
            .position(|candidate| *candidate == Some(host))
    }

    pub fn next_slot(&mut self) -> Result<usize, ActiveTargetError> {
        if N == 0 {
            return Err(ActiveTargetError::CapacityZero);
        }

        self.active_index = (self.active_index + 1) % N;
        Ok(self.active_index)
    }

    pub fn next_populated_slot(&mut self) -> Result<Option<usize>, ActiveTargetError> {
        if N == 0 {
            return Err(ActiveTargetError::CapacityZero);
        }
        if self.hosts.iter().all(Option::is_none) {
            return Ok(None);
        }

        for offset in 1..=N {
            let slot = (self.active_index + offset) % N;
            if self.hosts[slot].is_some() {
                self.active_index = slot;
                return Ok(Some(slot));
            }
        }

        Ok(None)
    }
}

impl<const N: usize> Default for HostRouter<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_only_to_active_slot() {
        let mut router = HostRouter::<3>::new();
        router.set_host(0, HostId(10)).unwrap();
        router.set_host(1, HostId(11)).unwrap();

        assert_eq!(router.active(), Some(HostId(10)));

        router.set_active_slot(1).unwrap();
        assert_eq!(router.active(), Some(HostId(11)));
    }

    #[test]
    fn cycles_slots_even_if_slot_is_unpaired() {
        let mut router = HostRouter::<2>::new();
        router.set_host(0, HostId(10)).unwrap();

        assert_eq!(router.next_slot(), Ok(1));
        assert_eq!(router.active(), None);
        assert_eq!(router.next_slot(), Ok(0));
        assert_eq!(router.active(), Some(HostId(10)));
    }

    #[test]
    fn restores_active_slot_from_saved_host_id() {
        let mut router = HostRouter::<3>::new();
        router.set_host(0, HostId(10)).unwrap();
        router.set_host(1, HostId(11)).unwrap();
        router.set_host(2, HostId(12)).unwrap();

        assert_eq!(router.set_active_host(HostId(12)), Ok(2));
        assert_eq!(router.active_slot(), Some(2));
        assert_eq!(router.active(), Some(HostId(12)));
    }

    #[test]
    fn saved_active_host_must_exist_in_known_slots() {
        let mut router = HostRouter::<2>::new();
        router.set_host(0, HostId(10)).unwrap();

        assert_eq!(
            router.set_active_host(HostId(99)),
            Err(ActiveTargetError::HostNotFound)
        );
        assert_eq!(router.active_slot(), Some(0));
        assert_eq!(router.active(), Some(HostId(10)));
    }

    #[test]
    fn cycles_to_next_populated_slot_for_physical_target_button() {
        let mut router = HostRouter::<4>::new();
        router.set_host(0, HostId(10)).unwrap();
        router.set_host(2, HostId(12)).unwrap();

        assert_eq!(router.next_populated_slot(), Ok(Some(2)));
        assert_eq!(router.active(), Some(HostId(12)));
        assert_eq!(router.next_populated_slot(), Ok(Some(0)));
        assert_eq!(router.active(), Some(HostId(10)));
    }

    #[test]
    fn populated_slot_cycle_reports_none_when_no_hosts_are_known() {
        let mut router = HostRouter::<3>::new();

        assert_eq!(router.next_populated_slot(), Ok(None));
        assert_eq!(router.active_slot(), Some(0));
        assert_eq!(router.active(), None);
    }
}
