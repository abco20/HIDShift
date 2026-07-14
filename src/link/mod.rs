//! Transport-independent, allocation-free protocol for the ESP32-to-ESP32 HID bridge.
//!
//! The wire format is deliberately smaller than an ESP-NOW v1 payload.  The
//! same packets can later be carried over UART without changing HID semantics.

mod composite;
mod input_record;
mod input_scheduler;
mod message;
mod sequence;
mod session;
mod snapshot;
mod state_refresh;
mod watchdog;
mod wire;

pub use composite::{
    COMPOSITE_DESCRIPTOR_MAX, CompositeDescriptor, CompositeDescriptorError, ReportIdMapping,
};
pub use input_record::InputReportRecord;
pub use input_scheduler::{
    InputDeliveryClass, InputScheduler, InputSchedulerError, InputSchedulerStats, ScheduledInput,
};
pub use message::{
    BridgeMessage, BridgeMessageError, BridgeRole, HidReportType, InputLane, InterfaceDescriptor,
    MAX_BRIDGE_MESSAGE_SIZE, MAX_HID_INTERFACES, MAX_HID_REPORT_DESCRIPTOR_SIZE,
    MAX_HID_REPORT_SIZE, MotionCumulative,
};
pub use sequence::{InputSequenceDecision, InputSequenceWindow, ReplayWindow};
pub use session::{SessionDecision, SessionHandshake};
pub use snapshot::{
    EncodedInputSnapshot, INPUT_SNAPSHOT_RECORD_HEADER_LEN, InputSnapshotHistory,
    InputSnapshotRecord, InputSnapshotRecords, REALTIME_CRITICAL_JOURNAL_CAPACITY, SnapshotError,
};
pub use state_refresh::{
    CRITICAL_STATE_REFRESH_COUNT, CRITICAL_STATE_REFRESH_INTERVAL_US, CriticalStateRefresh,
};
pub use watchdog::LinkWatchdog;
pub use wire::{
    BRIDGE_LINK_PROTOCOL_VERSION, ESP_NOW_PAYLOAD_MAX, FragmentEncoder, PacketFlags, PacketKind,
    Reassembler, ReassemblyError, WIRE_HEADER_LEN, WIRE_PAYLOAD_MAX, WirePacket, WirePacketError,
};
