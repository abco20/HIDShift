use heapless::Vec;

pub const DESTINATION_CAPACITY: usize = 8;
pub const SESSION_CAPACITY: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DestinationId(u8);

impl DestinationId {
    pub const fn new(value: u8) -> Option<Self> {
        if value >= 1 && value <= DESTINATION_CAPACITY as u8 {
            Some(Self(value))
        } else {
            None
        }
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SessionId(u8);

impl SessionId {
    pub const fn get(self) -> u8 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Destination {
    pub id: DestinationId,
    pub peer_identity: [u8; 16],
    pub auto_connect: bool,
    pub connected_session: Option<SessionId>,
    pub last_seen: u64,
}

impl Destination {
    pub const fn connected(&self) -> bool {
        self.connected_session.is_some()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationError {
    RegistryFull,
    SessionCapacity,
    UnknownDestination,
    AutoConnectDisabled,
    AlreadyConnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestinationRegistry {
    destinations: Vec<Destination, DESTINATION_CAPACITY>,
    sessions: [Option<DestinationId>; SESSION_CAPACITY],
}

impl Default for DestinationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DestinationRegistry {
    pub const fn new() -> Self {
        Self {
            destinations: Vec::new(),
            sessions: [None; SESSION_CAPACITY],
        }
    }

    pub fn destinations(&self) -> &[Destination] {
        &self.destinations
    }

    pub fn connected(&self) -> impl Iterator<Item = &Destination> {
        self.destinations.iter().filter(|item| item.connected())
    }

    pub fn registered(&self) -> impl Iterator<Item = &Destination> {
        self.destinations.iter().filter(|item| !item.connected())
    }

    pub fn register(
        &mut self,
        peer_identity: [u8; 16],
        now: u64,
    ) -> Result<DestinationId, DestinationError> {
        if let Some(existing) = self
            .destinations
            .iter_mut()
            .find(|item| item.peer_identity == peer_identity)
        {
            existing.last_seen = now;
            return Ok(existing.id);
        }
        let id = (1..=DESTINATION_CAPACITY as u8)
            .find_map(|value| {
                let id = DestinationId::new(value)?;
                (!self.destinations.iter().any(|item| item.id == id)).then_some(id)
            })
            .ok_or(DestinationError::RegistryFull)?;
        self.destinations
            .push(Destination {
                id,
                peer_identity,
                auto_connect: true,
                connected_session: None,
                last_seen: now,
            })
            .map_err(|_| DestinationError::RegistryFull)?;
        Ok(id)
    }

    pub fn set_auto_connect(
        &mut self,
        id: DestinationId,
        enabled: bool,
    ) -> Result<(), DestinationError> {
        let destination = self
            .destinations
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(DestinationError::UnknownDestination)?;
        destination.auto_connect = enabled;
        if !enabled {
            self.disconnect(id);
        }
        Ok(())
    }

    pub fn connect(&mut self, id: DestinationId, now: u64) -> Result<SessionId, DestinationError> {
        let destination = self
            .destinations
            .iter()
            .find(|item| item.id == id)
            .ok_or(DestinationError::UnknownDestination)?;
        if !destination.auto_connect {
            return Err(DestinationError::AutoConnectDisabled);
        }
        if destination.connected() {
            return Err(DestinationError::AlreadyConnected);
        }
        let index = self
            .sessions
            .iter()
            .position(Option::is_none)
            .ok_or(DestinationError::SessionCapacity)?;
        let session = SessionId(index as u8);
        self.sessions[index] = Some(id);
        let destination = self
            .destinations
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or(DestinationError::UnknownDestination)?;
        destination.connected_session = Some(session);
        destination.last_seen = now;
        Ok(session)
    }

    pub fn disconnect(&mut self, id: DestinationId) -> bool {
        let Some(destination) = self.destinations.iter_mut().find(|item| item.id == id) else {
            return false;
        };
        let Some(session) = destination.connected_session.take() else {
            return false;
        };
        self.sessions[session.get() as usize] = None;
        true
    }

    pub fn forget(&mut self, id: DestinationId) -> Result<Destination, DestinationError> {
        let index = self
            .destinations
            .iter()
            .position(|item| item.id == id)
            .ok_or(DestinationError::UnknownDestination)?;
        self.disconnect(id);
        Ok(self.destinations.swap_remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(value: u8) -> [u8; 16] {
        [value; 16]
    }

    #[test]
    fn eight_destinations_are_retained_but_only_four_sessions_connect() {
        let mut registry = DestinationRegistry::new();
        let mut ids = Vec::<DestinationId, DESTINATION_CAPACITY>::new();
        for value in 1..=8 {
            ids.push(registry.register(peer(value), value as u64).unwrap())
                .unwrap();
        }
        assert_eq!(
            registry.register(peer(9), 9),
            Err(DestinationError::RegistryFull)
        );
        for id in ids.iter().take(4) {
            registry.connect(*id, 20).unwrap();
        }
        assert_eq!(
            registry.connect(ids[4], 20),
            Err(DestinationError::SessionCapacity)
        );
        assert_eq!(registry.connected().count(), 4);
        assert_eq!(registry.registered().count(), 4);
    }

    #[test]
    fn disconnected_destination_returns_to_registered_list_and_keeps_identity() {
        let mut registry = DestinationRegistry::new();
        let id = registry.register(peer(1), 1).unwrap();
        registry.connect(id, 2).unwrap();
        assert!(registry.disconnect(id));
        assert_eq!(registry.connected().count(), 0);
        assert_eq!(registry.registered().next().unwrap().id, id);
    }

    #[test]
    fn disabling_auto_connect_disconnects_and_rejects_new_sessions() {
        let mut registry = DestinationRegistry::new();
        let id = registry.register(peer(1), 1).unwrap();
        registry.connect(id, 2).unwrap();
        registry.set_auto_connect(id, false).unwrap();
        assert_eq!(
            registry.connect(id, 3),
            Err(DestinationError::AutoConnectDisabled)
        );
        assert!(!registry.destinations()[0].connected());
    }

    #[test]
    fn registry_reuses_a_forgotten_id_without_moving_other_identities() {
        let mut registry = DestinationRegistry::new();
        let first = registry.register(peer(1), 1).unwrap();
        let second = registry.register(peer(2), 2).unwrap();
        registry.forget(first).unwrap();
        let replacement = registry.register(peer(3), 3).unwrap();
        assert_eq!(replacement, first);
        assert!(registry.destinations().iter().any(|item| item.id == second));
    }
}
