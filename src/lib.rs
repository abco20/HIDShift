#![cfg_attr(not(test), no_std)]

const fn parse_version_component(value: &str) -> u8 {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut result = 0u8;
    while index < bytes.len() {
        let digit = bytes[index];
        assert!(digit >= b'0' && digit <= b'9', "invalid package version");
        result = match result.checked_mul(10) {
            Some(value) => value,
            None => panic!("package version component exceeds u8"),
        };
        result = match result.checked_add(digit - b'0') {
            Some(value) => value,
            None => panic!("package version component exceeds u8"),
        };
        index += 1;
    }
    result
}

/// Firmware version reported through the management protocol.
pub const FIRMWARE_VERSION_MAJOR: u8 = parse_version_component(env!("CARGO_PKG_VERSION_MAJOR"));
/// Firmware version reported through the management protocol.
pub const FIRMWARE_VERSION_MINOR: u8 = parse_version_component(env!("CARGO_PKG_VERSION_MINOR"));
/// Firmware version reported through the management protocol.
pub const FIRMWARE_VERSION_PATCH: u8 = parse_version_component(env!("CARGO_PKG_VERSION_PATCH"));

pub mod ble;
pub mod ble_connection;
pub mod ble_notify;
pub mod ble_runtime;
pub mod bridge;
pub mod e2e;
pub mod ids;
pub mod input;
pub mod management;
pub mod mouse_accumulator;
pub mod output_target;
pub mod reports;
pub mod routing;
pub mod runtime;
pub mod settings;
pub mod storage;
pub mod target_control;
pub mod usb_hid;

pub use ble::{
    BleHidAttribute, BleHostAdapterError, BleHostAdapterEvent, bridge_events_from_ble_host_event,
    cccd_notify_enabled,
};
pub use ble_connection::{
    BLE_PAIRING_BACKOFF_STEPS_MS, BleConnectionEntry, BleConnectionSlot, BleConnectionSlotError,
    BleConnectionSlots, BleConnectionTiming, BleInputGate, BlePairingBackoff,
    BlePairingBackoffEntry, BlePeerIdentity, BlePhyPreference, low_latency_ble_connection_timing,
    resolve_host_id as resolve_ble_host_id, restrict_advertising_to_bonded_peers,
};
pub use ble_notify::{
    BleNotificationDispatchError, BleNotificationSink, BleTypedNotification,
    BleTypedNotificationError, dispatch_ble_task_command, dispatch_input_report_notifications,
    typed_notification,
};
pub use ble_runtime::{
    BleHidAttributeHandles, connected_message as ble_connected_message,
    disconnected_message as ble_disconnected_message, gatt_write_message as ble_gatt_write_message,
    security_changed_message as ble_security_changed_message,
};
pub use bridge::{
    BleHostStateMachine, Bridge, BridgeAction, BridgeError, BridgeEvent, BridgeStatus,
    HostRuntimeState, HostStateError, NotifyReason, PairingMode, PairingSession, ReportReady,
    keyboard_led_event_from_ble_output,
};
pub use ids::{
    DeviceId, HOST_SLOT_COUNT, HOST_SLOT_MAX, HOST_SLOT_MIN, HostId, HostSlot, InterfaceId,
    InvalidHostSlot, ReportId, SlotId,
};
pub use input::{
    ConsumerUsage, InputEvent, KeyCode, KeyUsage, KeyboardEvent, KeyboardFrame, KeyboardLedState,
    KeyboardSuppression, Modifier, ModifierState, MouseButton, MouseButtons, MouseFrame,
    MouseInputReport, MouseMovement, PhysicalInputState, PhysicalKeyboardState, PhysicalMouseState,
    StandardInputFrame, VisibleKeyboardState,
};
pub use management::{
    MANAGEMENT_PROTOCOL_VERSION, MANAGEMENT_REQUEST_LEN, MANAGEMENT_REQUEST_UUID,
    MANAGEMENT_RESPONSE_LEN, MANAGEMENT_RESPONSE_UUID, MANAGEMENT_SERVICE_UUID, ManagementCommand,
    ManagementDestination, ManagementDiagnostics, ManagementHistoryEvent, ManagementHostInfo,
    ManagementHostName, ManagementHostStatus, ManagementHostTiming, ManagementProtocolError,
    ManagementRequest, ManagementResponse, ManagementResponsePayload, ManagementResult,
    ManagementSchema, ManagementSetting, ManagementStatus, ManagementUsbDevice,
    ManagementUsbStatus,
};
pub use output_target::{
    MirrorCandidateId, MirrorConfiguration, OutputTarget, OutputTargetAvailability,
    OutputTargetState, StoredMirrorTarget, StoredOutputTarget, StoredPresentationConfig,
    UsbPresentation, effective_presentation,
};
pub use reports::{
    BLE_HID_INPUT_REPORT_MAX_LEN, BLE_HID_NOTIFICATIONS_PER_REPORT_MAX, BLE_HID_NOTIFY_MAX_LEN,
    BleConsumerReport, BleHidCharacteristic, BleHidInputReport, BleHidNotification,
    BleHidNotificationError, BleHidReport, BleKeyboard6KroReport, BleKeyboardLedOutputReport,
    BleKeyboardOutputError, BleKeyboardReport, BleMouseReport, ConsumerReport, FEATURE_REPORT_TYPE,
    HID_INFORMATION, INPUT_REPORT_TYPE, Keyboard6KroReport, KeyboardReportBuild, MouseReport,
    OUTPUT_REPORT_TYPE, ReportKind, StandardHidReport, V1_COMBINED_REPORT_MAP,
    notifications_for_input_report, report_id, report_type,
};
pub use routing::{ActiveTargetError, HostRouter};
pub use runtime::{
    BleCommandLane, BleTaskCommand, BleTaskCommandVec, BridgeRuntime, CommandClass,
    DEFAULT_RUNTIME_CAPACITIES, DefaultBridgeRuntime, DefaultRuntimeCommandQueues,
    ManagementTaskResponse, PairingModeState, RUNTIME_BLE_COMMAND_QUEUE_CAPACITY,
    RUNTIME_BLE_CONTROL_COMMAND_QUEUE_CAPACITY, RUNTIME_BLE_EVENT_CAPACITY,
    RUNTIME_BLE_GATT_WRITE_MAX_LEN, RUNTIME_BLE_NOTIFY_COMMAND_QUEUE_CAPACITY,
    RUNTIME_BRIDGE_ACTION_CAPACITY, RUNTIME_COMMAND_CAPACITY, RUNTIME_HOSTS_MAX,
    RUNTIME_INPUT_QUEUE_CAPACITY, RUNTIME_STATUS_COMMAND_QUEUE_CAPACITY,
    RUNTIME_STORAGE_COMMAND_QUEUE_CAPACITY, RUNTIME_USB_COMMAND_QUEUE_CAPACITY,
    RUNTIME_USB_INTERFACES_MAX, RuntimeCapacities, RuntimeCommand, RuntimeCommandQueues,
    RuntimeCommandVec, RuntimeCounters, RuntimeDiagnosticsEvent, RuntimeDispatchError,
    RuntimeError, RuntimeInput, StatusSnapshot, StatusTaskCommand, StatusTaskCommandVec,
    StorageTaskCommand, StorageTaskCommandVec, UsbHidInterfaceRuntimeState, UsbTaskCommand,
    UsbTaskCommandVec,
    bootstrap::prepare_ready_host,
    driver::{
        RuntimeDriverError, RuntimeTaskKind, RuntimeTaskSink, dispatch_runtime_queues,
        drive_runtime_message,
    },
    message::{
        RuntimeBleGattWrite, RuntimeBleHostEvent, RuntimeInputMessage, RuntimeInputMessageError,
    },
    owner::{DefaultRuntimeOwner, RuntimeOwner, RuntimeOwnerError},
};
#[cfg(feature = "dual-s3-wired")]
pub use runtime::{DeviceTaskCommand, RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY};
pub use settings::{
    GlobalSettings, HostSettings, SETTING_COUNT, SETTING_DESCRIPTORS, SETTINGS_SCHEMA_HASH,
    SETTINGS_SCHEMA_VERSION, SettingChoice, SettingDescriptor, SettingId, SettingScope,
    SettingTarget, SettingValueKind, setting_by_key, setting_descriptor, validate_setting_value,
};
pub use storage::{
    FixedName, NorFlashStorageBackend, STORAGE_FLASH_LEN, STORAGE_FLASH_SLOT_COUNT,
    STORAGE_FLASH_SLOT_SIZE, STORAGE_IMAGE_LEN, STORAGE_MAGIC, STORAGE_SCHEMA_VERSION,
    STORED_BOND_LEN, STORED_HOSTS_MAX, StorageDebouncer, StorageError, StorageFlashLayout,
    StorageHeader, StoragePersistPriority, StoragePersistence, StorageSlot, StorageSlotBackend,
    StorageSlotIndex, StorageState, StorageTaskAction, StorageTaskPolicy, StorageWriteResult,
    StoredAddressKind, StoredBond, StoredHostProfile, StoredSecurityLevel, decode_storage_image,
    encode_storage_image, persist_storage_state, restore_latest_storage_state,
    select_newest_valid_storage_image,
};
pub use target_control::{
    ButtonIntent, DebouncedButton, DebouncedButtonEvent, TargetSwitchControl,
};
pub use usb_hid::frame::{
    UsbInputFrameError, decode_standard_input_frame, events_to_standard_input_frame,
};
pub use usb_hid::host_interface::{HidInterfaceInfo, HidInterfaceLookupError, find_hid_interfaces};
pub use usb_hid::output::{
    BitPos, KeyboardLedOutputBytes, KeyboardLedOutputError, KeyboardLedOutputReport,
};
pub use usb_hid::runtime_adapter::runtime_input_from_usb_report;
pub use usb_hid::source::{
    OwnedUsbHidInputReport, USB_DEVICE_STRING_MAX_LEN, USB_HID_REPORT_DESCRIPTOR_MAX_LEN,
    USB_HID_REPORT_MAX_LEN, UsbDeviceString, UsbHidControlRequest, UsbHidControlRequestKind,
    UsbHidControlResponse, UsbHidControlResponseKind, UsbHidDeviceIdentity, UsbHidInputReport,
    UsbHidInterfaceSnapshot, UsbHidReportBytes, UsbHidReportDescriptorBytes, UsbHidReportTarget,
    UsbHidReportType, UsbHidSourceError, UsbHidSourceEvent,
};
pub use usb_hid::topology::{
    DefaultUsbTopologyManager, USB_TOPOLOGY_DEVICES_MAX, USB_TOPOLOGY_INTERFACES_MAX,
    UsbDeviceRoute, UsbDeviceTopologyEntry, UsbInterfaceTopologyEntry, UsbTopologyError,
    UsbTopologyManager, UsbTopologyRemoval,
};

#[cfg(test)]
mod version_tests {
    use super::{FIRMWARE_VERSION_MAJOR, FIRMWARE_VERSION_MINOR, FIRMWARE_VERSION_PATCH};

    #[test]
    fn firmware_version_comes_from_the_package_version() {
        assert_eq!(FIRMWARE_VERSION_MAJOR, 0);
        assert_eq!(FIRMWARE_VERSION_MINOR, 1);
        assert_eq!(FIRMWARE_VERSION_PATCH, 0);
    }
}
