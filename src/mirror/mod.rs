pub mod candidate;
pub mod control;
pub mod image;
pub mod parser;
pub mod plan;
pub mod store;
pub mod validator;

pub use candidate::{
    MirrorCandidateError, MirrorCandidateMetadata, MirrorCandidateRegistry,
    MirrorCandidateRegistryError, MirrorCandidateSource,
};
pub use control::{
    MIRROR_CONTROL_TIMEOUT_MS, MirrorControlForwarder, MirrorControlForwarderError,
    PendingMirrorControl,
};
pub use image::{
    HSMI_HEADER_LEN, HSMI_MAGIC, HSMI_MAX_SIZE, HSMI_VERSION, HidReportDescriptorTable,
    HidReportRecord, MirrorImage, MirrorImageEncodeError, MirrorImageHeader, MirrorImageSource,
    StringDescriptorTable, StringRecord, serialize_mirror_image,
};
pub use parser::{MirrorImageParseError, parse_mirror_image};
pub use plan::{
    EndpointPlan, HidInterfacePlan, MIRROR_ENDPOINTS_MAX, MIRROR_HID_INTERFACES_MAX,
    USB_DESCRIPTOR_BOS, USB_DESCRIPTOR_CONFIGURATION, USB_DESCRIPTOR_DEVICE, USB_DESCRIPTOR_HID,
    USB_DESCRIPTOR_HID_REPORT, USB_DESCRIPTOR_STRING, UsbDevicePlan,
};
pub use store::{
    MIRROR_PROFILE_PARTITION_LEN, ProfileCommitOutcome, ProfileSlot, ProfileStore,
    ProfileStoreBackend, ProfileStoreError, StoredProfile,
};
pub use validator::{MirrorRejectReason, validate_mirror_image};
