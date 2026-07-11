#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HostId(pub u8);

pub const HOST_SLOT_MIN: u8 = 1;
pub const HOST_SLOT_MAX: u8 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidHostSlot(pub u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct HostSlot(u8);

impl HostSlot {
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for HostSlot {
    type Error = InvalidHostSlot;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if (HOST_SLOT_MIN..=HOST_SLOT_MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidHostSlot(value))
        }
    }
}

impl From<HostSlot> for HostId {
    fn from(slot: HostSlot) -> Self {
        Self(slot.get())
    }
}

impl TryFrom<HostId> for HostSlot {
    type Error = InvalidHostSlot;

    fn try_from(host_id: HostId) -> Result<Self, Self::Error> {
        Self::try_from(host_id.0)
    }
}

impl HostId {
    pub fn validated(self) -> Result<HostSlot, InvalidHostSlot> {
        self.try_into()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct DeviceId(pub u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SlotId(pub u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct InterfaceId(pub u8);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ReportId(pub u8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_slot_rejects_zero_and_out_of_range_values() {
        assert_eq!(HostSlot::try_from(0), Err(InvalidHostSlot(0)));
        assert_eq!(HostSlot::try_from(5), Err(InvalidHostSlot(5)));
        assert_eq!(HostSlot::try_from(4).unwrap().get(), 4);
    }
}
