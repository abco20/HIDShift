use bitflags::bitflags;

bitflags! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub struct ChangeSet: u16 {
        const SUMMARY = 1 << 0;
        const SESSIONS = 1 << 1;
        const DESTINATIONS = 1 << 2;
        const INPUTS = 1 << 3;
        const SYSTEM = 1 << 4;
        const WIRED = 1 << 5;
        const SUPPORT = 1 << 6;
        const ALL = Self::SUMMARY.bits()
            | Self::SESSIONS.bits()
            | Self::DESTINATIONS.bits()
            | Self::INPUTS.bits()
            | Self::SYSTEM.bits()
            | Self::WIRED.bits()
            | Self::SUPPORT.bits();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DomainRevision(pub u32);

impl DomainRevision {
    fn advance(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Revisions {
    pub summary: DomainRevision,
    pub sessions: DomainRevision,
    pub destinations: DomainRevision,
    pub inputs: DomainRevision,
    pub system: DomainRevision,
    pub wired: DomainRevision,
    pub support: DomainRevision,
}

impl Revisions {
    pub fn changed(&mut self, changes: ChangeSet) {
        if changes.contains(ChangeSet::SUMMARY) {
            self.summary.advance();
        }
        if changes.contains(ChangeSet::SESSIONS) {
            self.sessions.advance();
        }
        if changes.contains(ChangeSet::DESTINATIONS) {
            self.destinations.advance();
        }
        if changes.contains(ChangeSet::INPUTS) {
            self.inputs.advance();
        }
        if changes.contains(ChangeSet::SYSTEM) {
            self.system.advance();
        }
        if changes.contains(ChangeSet::WIRED) {
            self.wired.advance();
        }
        if changes.contains(ChangeSet::SUPPORT) {
            self.support.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_changed_domains_advance() {
        let mut revisions = Revisions::default();
        revisions.changed(ChangeSet::SUMMARY | ChangeSet::INPUTS);
        assert_eq!(revisions.summary.0, 1);
        assert_eq!(revisions.inputs.0, 1);
        assert_eq!(revisions.destinations.0, 0);
    }
}
