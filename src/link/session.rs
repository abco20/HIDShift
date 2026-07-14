//! Small, allocation-free session handshake used by the ESP-NOW broadcast link.
//!
//! A Hello is authenticated with the pairing key, but it is not sufficient to
//! identify the current boot by itself: an old authenticated Hello can be
//! captured and replayed. The peer-session echo makes the handshake a
//! challenge/response exchange. Data is accepted only after both sides have
//! observed the current pair.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionDecision {
    Ignore,
    Reply,
    Established,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionHandshake {
    local_session: u32,
    peer_session: Option<u32>,
    pending_peer: Option<u32>,
    established: bool,
}

impl SessionHandshake {
    pub const fn new(local_session: u32) -> Self {
        Self {
            local_session,
            peer_session: None,
            pending_peer: None,
            established: false,
        }
    }

    pub const fn local_session(&self) -> u32 {
        self.local_session
    }

    pub const fn peer_session(&self) -> Option<u32> {
        self.peer_session
    }

    pub const fn is_established(&self) -> bool {
        self.established
    }

    /// Process a peer Hello and return whether a response or data activation
    /// is needed. A non-zero echo must always refer to our current session.
    pub fn observe(&mut self, peer_session: u32, echoed_local_session: u32) -> SessionDecision {
        if peer_session == 0
            || echoed_local_session != 0 && echoed_local_session != self.local_session
        {
            return SessionDecision::Ignore;
        }

        if echoed_local_session == 0 {
            if self.established && self.peer_session != Some(peer_session) {
                // An established link does not let an unauthenticated
                // challenge replace its active session. The owner may call
                // reset after link-loss before accepting a new boot.
                return SessionDecision::Ignore;
            }
            self.pending_peer = Some(peer_session);
            return SessionDecision::Reply;
        }

        if self.established {
            return if self.peer_session == Some(peer_session) {
                SessionDecision::Established
            } else {
                SessionDecision::Ignore
            };
        }

        if self.pending_peer == Some(peer_session) || self.peer_session.is_none() {
            self.peer_session = Some(peer_session);
            self.pending_peer = None;
            self.established = true;
            SessionDecision::Established
        } else {
            SessionDecision::Ignore
        }
    }

    pub fn reset(&mut self) {
        self.peer_session = None;
        self.pending_peer = None;
        self.established = false;
    }

    pub fn accepts_peer(&self, peer_session: u32) -> bool {
        self.established && self.peer_session == Some(peer_session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_echo_of_the_current_local_session() {
        let mut state = SessionHandshake::new(10);
        assert_eq!(state.observe(20, 0), SessionDecision::Reply);
        assert!(!state.is_established());
        assert_eq!(state.observe(20, 9), SessionDecision::Ignore);
        assert_eq!(state.observe(20, 10), SessionDecision::Established);
        assert!(state.accepts_peer(20));
    }

    #[test]
    fn established_session_cannot_be_replaced_by_a_replayed_hello() {
        let mut state = SessionHandshake::new(10);
        assert_eq!(state.observe(20, 0), SessionDecision::Reply);
        assert_eq!(state.observe(20, 10), SessionDecision::Established);
        assert_eq!(state.observe(3, 0), SessionDecision::Ignore);
        assert_eq!(state.observe(3, 10), SessionDecision::Ignore);
        assert!(state.accepts_peer(20));
    }

    #[test]
    fn reset_allows_peer_reboot_recovery() {
        let mut state = SessionHandshake::new(10);
        assert_eq!(state.observe(20, 0), SessionDecision::Reply);
        assert_eq!(state.observe(20, 10), SessionDecision::Established);
        state.reset();
        assert_eq!(state.observe(30, 0), SessionDecision::Reply);
        assert_eq!(state.observe(30, 10), SessionDecision::Established);
    }
}
