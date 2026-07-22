pub mod image;
pub mod parser;
pub mod plan;
pub mod store;
pub mod validator;

pub use image::{
    HSMI_HEADER_LEN, HSMI_MAGIC, HSMI_MAX_SIZE, HSMI_VERSION, HidReportDescriptorTable,
    HidReportRecord, MirrorImage, MirrorImageEncodeError, MirrorImageHeader, MirrorImageSource,
    StringDescriptorTable, StringRecord, serialize_mirror_image,
};
pub use parser::{MirrorImageParseError, parse_mirror_image};
pub use plan::{EndpointPlan, HidInterfacePlan, UsbDevicePlan};
pub use store::{
    MIRROR_PROFILE_PARTITION_LEN, ProfileCommitOutcome, ProfileSlot, ProfileStore,
    ProfileStoreBackend, ProfileStoreError, StoredProfile,
};
pub use validator::{MirrorRejectReason, validate_mirror_image};
