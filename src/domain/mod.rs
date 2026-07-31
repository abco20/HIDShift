//! Product-level concepts shared by firmware, management clients, and tests.
//!
//! These types deliberately do not expose BLE connection slots, USB interface
//! numbers, flash records, or transport details. Hardware adapters translate
//! to and from this boundary.

mod destination;
mod revision;
mod update;

pub use destination::{
    DESTINATION_CAPACITY, Destination, DestinationError, DestinationId, DestinationRegistry,
    SESSION_CAPACITY, SessionId,
};
pub use revision::{ChangeSet, DomainRevision, Revisions};
pub use update::{
    FirmwareNode, FirmwareUpdate, FirmwareUpdateError, FirmwareUpdatePhase, ImageMetadata,
    UpdateTarget,
};
