use crate::ids::HostId;
#[cfg(feature = "dual-s3-wired")]
use crate::output_target::{OutputTarget, OutputTargetAvailability, UsbPresentation};
use crate::settings::{SettingId, SettingTarget};

pub const MANAGEMENT_PROTOCOL_VERSION: u8 = 1;
pub const MANAGEMENT_REQUEST_LEN: usize = 20;
pub const MANAGEMENT_RESPONSE_LEN: usize = 20;
pub const MANAGEMENT_HOST_NAME_LEN: usize = 12;

pub const MANAGEMENT_SERVICE_UUID: &str = "7f510000-1b15-4f0d-9f4b-5b6d4f3a0001";
pub const MANAGEMENT_REQUEST_UUID: &str = "7f510001-1b15-4f0d-9f4b-5b6d4f3a0001";
pub const MANAGEMENT_RESPONSE_UUID: &str = "7f510002-1b15-4f0d-9f4b-5b6d4f3a0001";

const OP_GET_STATUS: u8 = 0x01;
const OP_SELECT_HOST: u8 = 0x02;
const OP_START_PAIRING: u8 = 0x03;
const OP_FORGET_HOST: u8 = 0x04;
const OP_GET_HOST_INFO: u8 = 0x05;
const OP_SET_HOST_NAME: u8 = 0x06;
const OP_CANCEL_PAIRING: u8 = 0x07;
const OP_GET_USB_DEVICE: u8 = 0x08;
const OP_GET_DIAGNOSTICS: u8 = 0x09;
const OP_GET_HISTORY: u8 = 0x0a;
const OP_GET_SCHEMA: u8 = 0x0b;
const OP_GET_SETTING: u8 = 0x0c;
const OP_SET_SETTING: u8 = 0x0d;
const OP_GET_HOST_TIMING: u8 = 0x0e;
#[cfg(feature = "dual-s3-wired")]
const OP_SELECT_OUTPUT_TARGET: u8 = 0x0f;
#[cfg(feature = "dual-s3-wired")]
const OP_GET_OUTPUT_TARGET_STATUS: u8 = 0x10;

const PAYLOAD_NONE: u8 = 0;
const PAYLOAD_STATUS: u8 = 1;
const PAYLOAD_HOST_INFO: u8 = 2;
const PAYLOAD_USB_DEVICE: u8 = 3;
const PAYLOAD_DIAGNOSTICS: u8 = 4;
const PAYLOAD_HISTORY: u8 = 5;
const PAYLOAD_SCHEMA: u8 = 6;
const PAYLOAD_SETTING: u8 = 7;
const PAYLOAD_HOST_TIMING: u8 = 8;
#[cfg(feature = "dual-s3-wired")]
const PAYLOAD_OUTPUT_TARGET_STATUS: u8 = 9;
const STATUS_PAYLOAD_LEN: u8 = 10;
const HOST_INFO_PAYLOAD_LEN: u8 = 15;
const USB_DEVICE_PAYLOAD_LEN: u8 = 15;
const DIAGNOSTICS_PAYLOAD_LEN: u8 = 15;
const HISTORY_PAYLOAD_LEN: u8 = 15;
const SCHEMA_PAYLOAD_LEN: u8 = 8;
const SETTING_PAYLOAD_LEN: u8 = 8;
const HOST_TIMING_PAYLOAD_LEN: u8 = 10;
#[cfg(feature = "dual-s3-wired")]
const OUTPUT_TARGET_STATUS_PAYLOAD_LEN: u8 = 14;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementRequest {
    pub request_id: u8,
    pub command: ManagementCommand,
}

impl ManagementRequest {
    pub fn decode(bytes: &[u8]) -> Result<Self, ManagementProtocolError> {
        if bytes.len() != MANAGEMENT_REQUEST_LEN {
            return Err(ManagementProtocolError::InvalidLength);
        }
        if bytes[0] != MANAGEMENT_PROTOCOL_VERSION {
            return Err(ManagementProtocolError::UnsupportedVersion);
        }
        let payload_len = bytes[3] as usize;
        if payload_len > MANAGEMENT_REQUEST_LEN - 4 {
            return Err(ManagementProtocolError::InvalidLength);
        }
        let payload = &bytes[4..4 + payload_len];
        let command = match (bytes[2], payload) {
            (OP_GET_STATUS, []) => ManagementCommand::GetStatus,
            (OP_SELECT_HOST, [host]) => ManagementCommand::SelectHost(HostId(*host)),
            (OP_START_PAIRING, [host]) => ManagementCommand::StartPairing(HostId(*host)),
            (OP_FORGET_HOST, [host]) => ManagementCommand::ForgetHost(HostId(*host)),
            (OP_GET_HOST_INFO, [host]) => ManagementCommand::GetHostInfo(HostId(*host)),
            (OP_CANCEL_PAIRING, []) => ManagementCommand::CancelPairing,
            (OP_GET_USB_DEVICE, [index, name_offset]) => ManagementCommand::GetUsbDevice {
                index: *index,
                name_offset: *name_offset,
            },
            (OP_GET_DIAGNOSTICS, []) => ManagementCommand::GetDiagnostics,
            (OP_GET_HOST_TIMING, [host]) => ManagementCommand::GetHostTiming(HostId(*host)),
            #[cfg(feature = "dual-s3-wired")]
            (OP_SELECT_OUTPUT_TARGET, [kind, id]) => {
                ManagementCommand::SelectOutputTarget(ManagementOutputTarget::decode(*kind, *id)?)
            }
            #[cfg(feature = "dual-s3-wired")]
            (OP_GET_OUTPUT_TARGET_STATUS, []) => ManagementCommand::GetOutputTargetStatus,
            (OP_GET_HISTORY, [index]) => ManagementCommand::GetHistory { index: *index },
            (OP_GET_SCHEMA, []) => ManagementCommand::GetSchema,
            (OP_GET_SETTING, [id_low, id_high, scope, target]) => ManagementCommand::GetSetting {
                id: SettingId::from_u16(u16::from_le_bytes([*id_low, *id_high]))
                    .ok_or(ManagementProtocolError::InvalidArgument)?,
                target: decode_setting_target(*scope, *target)?,
            },
            (OP_SET_SETTING, [id_low, id_high, scope, target, value @ ..]) if value.len() == 4 => {
                ManagementCommand::SetSetting {
                    id: SettingId::from_u16(u16::from_le_bytes([*id_low, *id_high]))
                        .ok_or(ManagementProtocolError::InvalidArgument)?,
                    target: decode_setting_target(*scope, *target)?,
                    value: i32::from_le_bytes([value[0], value[1], value[2], value[3]]),
                }
            }
            (OP_SET_HOST_NAME, payload) if payload.len() == 2 + MANAGEMENT_HOST_NAME_LEN => {
                let length = payload[1] as usize;
                if length > MANAGEMENT_HOST_NAME_LEN {
                    return Err(ManagementProtocolError::InvalidArgument);
                }
                let mut bytes = [0; MANAGEMENT_HOST_NAME_LEN];
                bytes.copy_from_slice(&payload[2..]);
                let name = ManagementHostName::from_parts(length as u8, bytes)?;
                ManagementCommand::SetHostName {
                    host_id: HostId(payload[0]),
                    name,
                }
            }
            (
                OP_GET_STATUS | OP_SELECT_HOST | OP_START_PAIRING | OP_FORGET_HOST
                | OP_GET_HOST_INFO | OP_SET_HOST_NAME | OP_CANCEL_PAIRING | OP_GET_USB_DEVICE
                | OP_GET_DIAGNOSTICS | OP_GET_HISTORY | OP_GET_SCHEMA | OP_GET_SETTING
                | OP_SET_SETTING | OP_GET_HOST_TIMING,
                _,
            ) => {
                return Err(ManagementProtocolError::InvalidArgument);
            }
            #[cfg(feature = "dual-s3-wired")]
            (OP_SELECT_OUTPUT_TARGET | OP_GET_OUTPUT_TARGET_STATUS, _) => {
                return Err(ManagementProtocolError::InvalidArgument);
            }
            _ => return Err(ManagementProtocolError::UnknownCommand),
        };
        Ok(Self {
            request_id: bytes[1],
            command,
        })
    }

    pub fn encode(self) -> [u8; MANAGEMENT_REQUEST_LEN] {
        let mut bytes = [0; MANAGEMENT_REQUEST_LEN];
        bytes[0] = MANAGEMENT_PROTOCOL_VERSION;
        bytes[1] = self.request_id;
        match self.command {
            ManagementCommand::GetStatus => bytes[2] = OP_GET_STATUS,
            ManagementCommand::SelectHost(host_id) => {
                encode_host_command(&mut bytes, OP_SELECT_HOST, host_id)
            }
            ManagementCommand::StartPairing(host_id) => {
                encode_host_command(&mut bytes, OP_START_PAIRING, host_id)
            }
            ManagementCommand::ForgetHost(host_id) => {
                encode_host_command(&mut bytes, OP_FORGET_HOST, host_id)
            }
            ManagementCommand::GetHostInfo(host_id) => {
                encode_host_command(&mut bytes, OP_GET_HOST_INFO, host_id)
            }
            ManagementCommand::CancelPairing => bytes[2] = OP_CANCEL_PAIRING,
            ManagementCommand::GetUsbDevice { index, name_offset } => {
                bytes[2] = OP_GET_USB_DEVICE;
                bytes[3] = 2;
                bytes[4] = index;
                bytes[5] = name_offset;
            }
            ManagementCommand::GetDiagnostics => bytes[2] = OP_GET_DIAGNOSTICS,
            ManagementCommand::GetHostTiming(host_id) => {
                encode_host_command(&mut bytes, OP_GET_HOST_TIMING, host_id)
            }
            #[cfg(feature = "dual-s3-wired")]
            ManagementCommand::SelectOutputTarget(target) => {
                bytes[2] = OP_SELECT_OUTPUT_TARGET;
                bytes[3] = 2;
                let (kind, id) = target.encode();
                bytes[4] = kind;
                bytes[5] = id;
            }
            #[cfg(feature = "dual-s3-wired")]
            ManagementCommand::GetOutputTargetStatus => bytes[2] = OP_GET_OUTPUT_TARGET_STATUS,
            ManagementCommand::GetHistory { index } => {
                bytes[2] = OP_GET_HISTORY;
                bytes[3] = 1;
                bytes[4] = index;
            }
            ManagementCommand::GetSchema => bytes[2] = OP_GET_SCHEMA,
            ManagementCommand::GetSetting { id, target } => {
                bytes[2] = OP_GET_SETTING;
                bytes[3] = 4;
                bytes[4..6].copy_from_slice(&(id as u16).to_le_bytes());
                encode_setting_target(&mut bytes[6..8], target);
            }
            ManagementCommand::SetSetting { id, target, value } => {
                bytes[2] = OP_SET_SETTING;
                bytes[3] = 8;
                bytes[4..6].copy_from_slice(&(id as u16).to_le_bytes());
                encode_setting_target(&mut bytes[6..8], target);
                bytes[8..12].copy_from_slice(&value.to_le_bytes());
            }
            ManagementCommand::SetHostName { host_id, name } => {
                bytes[2] = OP_SET_HOST_NAME;
                bytes[3] = (2 + MANAGEMENT_HOST_NAME_LEN) as u8;
                bytes[4] = host_id.0;
                bytes[5] = name.len;
                bytes[6..6 + MANAGEMENT_HOST_NAME_LEN].copy_from_slice(&name.bytes);
            }
        }
        bytes
    }
}

fn encode_host_command(bytes: &mut [u8; MANAGEMENT_REQUEST_LEN], opcode: u8, host_id: HostId) {
    bytes[2] = opcode;
    bytes[3] = 1;
    bytes[4] = host_id.0;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementCommand {
    GetStatus,
    SelectHost(HostId),
    StartPairing(HostId),
    ForgetHost(HostId),
    GetHostInfo(HostId),
    SetHostName {
        host_id: HostId,
        name: ManagementHostName,
    },
    CancelPairing,
    GetUsbDevice {
        index: u8,
        name_offset: u8,
    },
    GetDiagnostics,
    GetHostTiming(HostId),
    GetHistory {
        index: u8,
    },
    GetSchema,
    GetSetting {
        id: SettingId,
        target: SettingTarget,
    },
    SetSetting {
        id: SettingId,
        target: SettingTarget,
        value: i32,
    },
    #[cfg(feature = "dual-s3-wired")]
    SelectOutputTarget(ManagementOutputTarget),
    #[cfg(feature = "dual-s3-wired")]
    GetOutputTargetStatus,
}

#[cfg(feature = "dual-s3-wired")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementOutputTarget {
    Wired,
    Ble(HostId),
}

#[cfg(feature = "dual-s3-wired")]
impl ManagementOutputTarget {
    const fn encode(self) -> (u8, u8) {
        match self {
            Self::Wired => (0, 0),
            Self::Ble(host_id) => (1, host_id.0),
        }
    }

    fn decode(kind: u8, id: u8) -> Result<Self, ManagementProtocolError> {
        match (kind, id) {
            (0, 0) => Ok(Self::Wired),
            (1, 1..=4) => Ok(Self::Ble(HostId(id))),
            _ => Err(ManagementProtocolError::InvalidArgument),
        }
    }

    pub const fn to_output_target(self) -> OutputTarget {
        match self {
            Self::Wired => OutputTarget::Wired,
            Self::Ble(host_id) => OutputTarget::Ble(host_id),
        }
    }
}

#[cfg(feature = "dual-s3-wired")]
impl From<OutputTarget> for ManagementOutputTarget {
    fn from(value: OutputTarget) -> Self {
        match value {
            OutputTarget::Wired => Self::Wired,
            OutputTarget::Ble(host_id) => Self::Ble(host_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementDestination {
    Wired,
    Ble(HostId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementResponse {
    pub request_id: u8,
    pub result: ManagementResult,
    pub payload: ManagementResponsePayload,
}

impl ManagementResponse {
    pub fn encode(self) -> [u8; MANAGEMENT_RESPONSE_LEN] {
        let mut bytes = [0; MANAGEMENT_RESPONSE_LEN];
        bytes[0] = MANAGEMENT_PROTOCOL_VERSION;
        bytes[1] = self.request_id;
        bytes[2] = self.result as u8;
        match self.payload {
            ManagementResponsePayload::None => bytes[3] = PAYLOAD_NONE,
            ManagementResponsePayload::Status(status) => {
                bytes[3] = PAYLOAD_STATUS;
                bytes[4] = STATUS_PAYLOAD_LEN;
                bytes[5] = option_host_id(status.active_host);
                bytes[6] = option_host_id(status.pairing_host);
                bytes[7] = status.host_count;
                bytes[8] = status.usb.device_count;
                bytes[9] = status.usb.interface_count;
                bytes[10] = status.usb.keyboard_count;
                for (index, host) in status.hosts.iter().enumerate() {
                    bytes[11 + index] = host.bits();
                }
            }
            ManagementResponsePayload::HostInfo(info) => {
                bytes[3] = PAYLOAD_HOST_INFO;
                bytes[4] = HOST_INFO_PAYLOAD_LEN;
                bytes[5] = info.host_id.0;
                bytes[6] = info.status.bits() | ((info.name_source & 0x03) << 4);
                bytes[7] = info.name.len;
                bytes[8..20].copy_from_slice(&info.name.bytes);
            }
            ManagementResponsePayload::UsbDevice(device) => {
                bytes[3] = PAYLOAD_USB_DEVICE;
                bytes[4] = USB_DEVICE_PAYLOAD_LEN;
                bytes[5] = device.index;
                bytes[6] = device.device_id;
                bytes[7] = device.flags;
                bytes[8..10].copy_from_slice(&device.vendor_id.to_le_bytes());
                bytes[10..12].copy_from_slice(&device.product_id.to_le_bytes());
                bytes[12] = device.name_len;
                bytes[13] = device.name_offset;
                bytes[14] = device.name_chunk_len;
                bytes[15..20].copy_from_slice(&device.name_chunk);
            }
            ManagementResponsePayload::Diagnostics(diagnostics) => {
                bytes[3] = PAYLOAD_DIAGNOSTICS;
                bytes[4] = DIAGNOSTICS_PAYLOAD_LEN;
                bytes[5..9].copy_from_slice(&diagnostics.uptime_seconds.to_le_bytes());
                bytes[9] = diagnostics.reset_reason;
                bytes[10..12].copy_from_slice(&diagnostics.brownout_count.to_le_bytes());
                bytes[12..14].copy_from_slice(&diagnostics.ble_disconnect_count.to_le_bytes());
                bytes[14..16].copy_from_slice(&diagnostics.ble_notify_failure_count.to_le_bytes());
                bytes[16..18].copy_from_slice(&diagnostics.usb_error_count.to_le_bytes());
                bytes[18] = diagnostics.flash_write_count;
                bytes[19] = diagnostics.flash_failure_count;
            }
            ManagementResponsePayload::History(event) => {
                bytes[3] = PAYLOAD_HISTORY;
                bytes[4] = HISTORY_PAYLOAD_LEN;
                bytes[5] = event.kind;
                bytes[6..8].copy_from_slice(&event.sequence.to_le_bytes());
                bytes[8..12].copy_from_slice(&event.timestamp_seconds.to_le_bytes());
                bytes[12] = event.subject;
                bytes[13] = event.detail;
                bytes[14..16].copy_from_slice(&event.vendor_id.to_le_bytes());
                bytes[16..18].copy_from_slice(&event.product_id.to_le_bytes());
            }
            ManagementResponsePayload::Schema(schema) => {
                bytes[3] = PAYLOAD_SCHEMA;
                bytes[4] = SCHEMA_PAYLOAD_LEN;
                bytes[5..7].copy_from_slice(&schema.version.to_le_bytes());
                bytes[7] = schema.setting_count;
                bytes[8] = schema.history_capacity;
                bytes[9] = schema.usb_capacity;
                bytes[10..14].copy_from_slice(&schema.hash.to_le_bytes());
                bytes[14] = schema.firmware_major;
                bytes[15] = schema.firmware_minor;
                bytes[16] = schema.firmware_patch;
            }
            ManagementResponsePayload::Setting(setting) => {
                bytes[3] = PAYLOAD_SETTING;
                bytes[4] = SETTING_PAYLOAD_LEN;
                bytes[5..7].copy_from_slice(&(setting.id as u16).to_le_bytes());
                encode_setting_target(&mut bytes[7..9], setting.target);
                bytes[9..13].copy_from_slice(&setting.value.to_le_bytes());
            }
            ManagementResponsePayload::HostTiming(timing) => {
                bytes[3] = PAYLOAD_HOST_TIMING;
                bytes[4] = HOST_TIMING_PAYLOAD_LEN;
                bytes[5] = timing.host_id.0;
                bytes[6..10].copy_from_slice(&timing.last_connected_seconds.to_le_bytes());
                bytes[10..14].copy_from_slice(&timing.last_disconnected_seconds.to_le_bytes());
                bytes[14] = timing.last_disconnect_reason;
            }
            #[cfg(feature = "dual-s3-wired")]
            ManagementResponsePayload::OutputTargetStatus(status) => {
                bytes[3] = PAYLOAD_OUTPUT_TARGET_STATUS;
                bytes[4] = OUTPUT_TARGET_STATUS_PAYLOAD_LEN;
                let (selected_kind, selected_id) = status.selected.encode();
                bytes[5] = selected_kind;
                bytes[6] = selected_id;
                if let Some(active) = status.active {
                    let (active_kind, active_id) = active.encode();
                    bytes[7] = active_kind;
                    bytes[8] = active_id;
                } else {
                    bytes[7] = 0xff;
                }
                bytes[9] = availability_byte(status.availability);
                bytes[10] = u8::from(status.wired_ready);
                bytes[11] = status.ready_ble_mask;
                bytes[12] = status.effective_presentation as u8;
                bytes[13] = u8::from(status.mirror_configured);
                bytes[14..18].copy_from_slice(&status.operation_id.to_le_bytes());
            }
        }
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ManagementProtocolError> {
        if bytes.len() != MANAGEMENT_RESPONSE_LEN {
            return Err(ManagementProtocolError::InvalidLength);
        }
        if bytes[0] != MANAGEMENT_PROTOCOL_VERSION {
            return Err(ManagementProtocolError::UnsupportedVersion);
        }
        let result = ManagementResult::from_byte(bytes[2])?;
        let payload = match (bytes[3], bytes[4]) {
            (PAYLOAD_NONE, 0) => ManagementResponsePayload::None,
            (PAYLOAD_STATUS, STATUS_PAYLOAD_LEN) => {
                if bytes[7] > 4 {
                    return Err(ManagementProtocolError::InvalidArgument);
                }
                ManagementResponsePayload::Status(ManagementStatus {
                    active_host: decode_optional_host(bytes[5]),
                    pairing_host: decode_optional_host(bytes[6]),
                    host_count: bytes[7],
                    usb: ManagementUsbStatus {
                        device_count: bytes[8],
                        interface_count: bytes[9],
                        keyboard_count: bytes[10],
                    },
                    hosts: [
                        ManagementHostStatus::from_bits(bytes[11]),
                        ManagementHostStatus::from_bits(bytes[12]),
                        ManagementHostStatus::from_bits(bytes[13]),
                        ManagementHostStatus::from_bits(bytes[14]),
                    ],
                })
            }
            (PAYLOAD_HOST_INFO, HOST_INFO_PAYLOAD_LEN) => {
                let mut name = [0; MANAGEMENT_HOST_NAME_LEN];
                name.copy_from_slice(&bytes[8..20]);
                ManagementResponsePayload::HostInfo(ManagementHostInfo {
                    host_id: HostId(bytes[5]),
                    status: ManagementHostStatus::from_bits(bytes[6]),
                    name_source: (bytes[6] >> 4) & 0x03,
                    name: ManagementHostName::from_parts(bytes[7], name)?,
                })
            }
            (PAYLOAD_USB_DEVICE, USB_DEVICE_PAYLOAD_LEN) => {
                let mut name_chunk = [0; 5];
                name_chunk.copy_from_slice(&bytes[15..20]);
                if bytes[14] > 5 || bytes[13].saturating_add(bytes[14]) > bytes[12] {
                    return Err(ManagementProtocolError::InvalidArgument);
                }
                ManagementResponsePayload::UsbDevice(ManagementUsbDevice {
                    index: bytes[5],
                    device_id: bytes[6],
                    flags: bytes[7],
                    vendor_id: u16::from_le_bytes([bytes[8], bytes[9]]),
                    product_id: u16::from_le_bytes([bytes[10], bytes[11]]),
                    name_len: bytes[12],
                    name_offset: bytes[13],
                    name_chunk_len: bytes[14],
                    name_chunk,
                })
            }
            (PAYLOAD_DIAGNOSTICS, DIAGNOSTICS_PAYLOAD_LEN) => {
                ManagementResponsePayload::Diagnostics(ManagementDiagnostics {
                    uptime_seconds: u32::from_le_bytes([bytes[5], bytes[6], bytes[7], bytes[8]]),
                    reset_reason: bytes[9],
                    brownout_count: u16::from_le_bytes([bytes[10], bytes[11]]),
                    ble_disconnect_count: u16::from_le_bytes([bytes[12], bytes[13]]),
                    ble_notify_failure_count: u16::from_le_bytes([bytes[14], bytes[15]]),
                    usb_error_count: u16::from_le_bytes([bytes[16], bytes[17]]),
                    flash_write_count: bytes[18],
                    flash_failure_count: bytes[19],
                })
            }
            (PAYLOAD_HISTORY, HISTORY_PAYLOAD_LEN) => {
                ManagementResponsePayload::History(ManagementHistoryEvent {
                    kind: bytes[5],
                    sequence: u16::from_le_bytes([bytes[6], bytes[7]]),
                    timestamp_seconds: u32::from_le_bytes([
                        bytes[8], bytes[9], bytes[10], bytes[11],
                    ]),
                    subject: bytes[12],
                    detail: bytes[13],
                    vendor_id: u16::from_le_bytes([bytes[14], bytes[15]]),
                    product_id: u16::from_le_bytes([bytes[16], bytes[17]]),
                })
            }
            (PAYLOAD_SCHEMA, SCHEMA_PAYLOAD_LEN) => {
                ManagementResponsePayload::Schema(ManagementSchema {
                    version: u16::from_le_bytes([bytes[5], bytes[6]]),
                    setting_count: bytes[7],
                    history_capacity: bytes[8],
                    usb_capacity: bytes[9],
                    hash: u32::from_le_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]),
                    firmware_major: bytes[14],
                    firmware_minor: bytes[15],
                    firmware_patch: bytes[16],
                })
            }
            (PAYLOAD_SETTING, SETTING_PAYLOAD_LEN) => {
                let id = SettingId::from_u16(u16::from_le_bytes([bytes[5], bytes[6]]))
                    .ok_or(ManagementProtocolError::InvalidArgument)?;
                ManagementResponsePayload::Setting(ManagementSetting {
                    id,
                    target: decode_setting_target(bytes[7], bytes[8])?,
                    value: i32::from_le_bytes([bytes[9], bytes[10], bytes[11], bytes[12]]),
                })
            }
            (PAYLOAD_HOST_TIMING, HOST_TIMING_PAYLOAD_LEN) => {
                ManagementResponsePayload::HostTiming(ManagementHostTiming {
                    host_id: HostId(bytes[5]),
                    last_connected_seconds: u32::from_le_bytes([
                        bytes[6], bytes[7], bytes[8], bytes[9],
                    ]),
                    last_disconnected_seconds: u32::from_le_bytes([
                        bytes[10], bytes[11], bytes[12], bytes[13],
                    ]),
                    last_disconnect_reason: bytes[14],
                })
            }
            #[cfg(feature = "dual-s3-wired")]
            (PAYLOAD_OUTPUT_TARGET_STATUS, OUTPUT_TARGET_STATUS_PAYLOAD_LEN) => {
                let selected = ManagementOutputTarget::decode(bytes[5], bytes[6])?;
                let active = if bytes[7] == 0xff {
                    if bytes[8] != 0 {
                        return Err(ManagementProtocolError::InvalidArgument);
                    }
                    None
                } else {
                    Some(ManagementOutputTarget::decode(bytes[7], bytes[8])?)
                };
                ManagementResponsePayload::OutputTargetStatus(ManagementOutputTargetStatus {
                    selected,
                    active,
                    availability: availability_from_byte(bytes[9])?,
                    wired_ready: decode_bool(bytes[10])?,
                    ready_ble_mask: bytes[11] & 0x0f,
                    effective_presentation: ManagementUsbPresentationKind::from_byte(bytes[12])?,
                    mirror_configured: decode_bool(bytes[13])?,
                    operation_id: u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]),
                })
            }
            _ => return Err(ManagementProtocolError::InvalidArgument),
        };
        Ok(Self {
            request_id: bytes[1],
            result,
            payload,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementResponsePayload {
    None,
    Status(ManagementStatus),
    HostInfo(ManagementHostInfo),
    UsbDevice(ManagementUsbDevice),
    Diagnostics(ManagementDiagnostics),
    History(ManagementHistoryEvent),
    Schema(ManagementSchema),
    Setting(ManagementSetting),
    HostTiming(ManagementHostTiming),
    #[cfg(feature = "dual-s3-wired")]
    OutputTargetStatus(ManagementOutputTargetStatus),
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementResult {
    Ok = 0,
    InvalidHost = 1,
    HostNotFound = 2,
    HostAlreadyBonded = 3,
    InternalError = 4,
    InvalidName = 5,
    InvalidSetting = 6,
    NotFound = 7,
    Unavailable = 8,
}

impl ManagementResult {
    fn from_byte(value: u8) -> Result<Self, ManagementProtocolError> {
        match value {
            0 => Ok(Self::Ok),
            1 => Ok(Self::InvalidHost),
            2 => Ok(Self::HostNotFound),
            3 => Ok(Self::HostAlreadyBonded),
            4 => Ok(Self::InternalError),
            5 => Ok(Self::InvalidName),
            6 => Ok(Self::InvalidSetting),
            7 => Ok(Self::NotFound),
            8 => Ok(Self::Unavailable),
            _ => Err(ManagementProtocolError::InvalidArgument),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementStatus {
    pub active_host: Option<HostId>,
    pub pairing_host: Option<HostId>,
    pub host_count: u8,
    pub usb: ManagementUsbStatus,
    pub hosts: [ManagementHostStatus; 4],
}

impl ManagementStatus {
    pub const fn empty(host_count: u8) -> Self {
        Self {
            active_host: None,
            pairing_host: None,
            host_count,
            usb: ManagementUsbStatus::empty(),
            hosts: [ManagementHostStatus::empty(); 4],
        }
    }
}

#[cfg(feature = "dual-s3-wired")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementOutputTargetStatus {
    pub selected: ManagementOutputTarget,
    pub active: Option<ManagementOutputTarget>,
    pub availability: OutputTargetAvailability,
    pub wired_ready: bool,
    pub ready_ble_mask: u8,
    pub effective_presentation: ManagementUsbPresentationKind,
    pub mirror_configured: bool,
    pub operation_id: u32,
}

#[cfg(feature = "dual-s3-wired")]
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementUsbPresentationKind {
    Fallback = 0,
    Mirror = 1,
}

#[cfg(feature = "dual-s3-wired")]
impl ManagementUsbPresentationKind {
    fn from_byte(value: u8) -> Result<Self, ManagementProtocolError> {
        match value {
            0 => Ok(Self::Fallback),
            1 => Ok(Self::Mirror),
            _ => Err(ManagementProtocolError::InvalidArgument),
        }
    }
}

#[cfg(feature = "dual-s3-wired")]
impl From<UsbPresentation> for ManagementUsbPresentationKind {
    fn from(value: UsbPresentation) -> Self {
        match value {
            UsbPresentation::Fallback => Self::Fallback,
            UsbPresentation::Mirror(_) => Self::Mirror,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementUsbStatus {
    pub device_count: u8,
    pub interface_count: u8,
    pub keyboard_count: u8,
}

pub const MANAGEMENT_USB_NAME_CHUNK_LEN: usize = 5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementUsbDevice {
    pub index: u8,
    pub device_id: u8,
    /// bit 0 connected, bit 1 keyboard, bit 2 mouse, bit 3 consumer control.
    pub flags: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub name_len: u8,
    pub name_offset: u8,
    pub name_chunk_len: u8,
    pub name_chunk: [u8; MANAGEMENT_USB_NAME_CHUNK_LEN],
}

impl ManagementUsbDevice {
    pub fn name_chunk(&self) -> &[u8] {
        &self.name_chunk[..self.name_chunk_len as usize]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct ManagementDiagnostics {
    pub uptime_seconds: u32,
    pub reset_reason: u8,
    pub brownout_count: u16,
    pub ble_disconnect_count: u16,
    pub ble_notify_failure_count: u16,
    pub usb_error_count: u16,
    pub flash_write_count: u8,
    pub flash_failure_count: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementHistoryEvent {
    pub kind: u8,
    pub sequence: u16,
    pub timestamp_seconds: u32,
    pub subject: u8,
    pub detail: u8,
    pub vendor_id: u16,
    pub product_id: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementSchema {
    pub version: u16,
    pub setting_count: u8,
    pub history_capacity: u8,
    pub usb_capacity: u8,
    pub hash: u32,
    pub firmware_major: u8,
    pub firmware_minor: u8,
    pub firmware_patch: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementSetting {
    pub id: SettingId,
    pub target: SettingTarget,
    pub value: i32,
}

impl ManagementUsbStatus {
    pub const fn empty() -> Self {
        Self {
            device_count: 0,
            interface_count: 0,
            keyboard_count: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementHostInfo {
    pub host_id: HostId,
    pub status: ManagementHostStatus,
    pub name: ManagementHostName,
    /// 0 unknown, 1 automatically discovered, 2 manually overridden.
    pub name_source: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementHostTiming {
    pub host_id: HostId,
    pub last_connected_seconds: u32,
    pub last_disconnected_seconds: u32,
    pub last_disconnect_reason: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementHostName {
    len: u8,
    bytes: [u8; MANAGEMENT_HOST_NAME_LEN],
}

impl ManagementHostName {
    pub const fn empty() -> Self {
        Self {
            len: 0,
            bytes: [0; MANAGEMENT_HOST_NAME_LEN],
        }
    }

    pub fn from_ascii(name: &str) -> Result<Self, ManagementProtocolError> {
        if name.len() > MANAGEMENT_HOST_NAME_LEN
            || !name
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
        {
            return Err(ManagementProtocolError::InvalidArgument);
        }
        let mut bytes = [0; MANAGEMENT_HOST_NAME_LEN];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Self {
            len: name.len() as u8,
            bytes,
        })
    }

    fn from_parts(
        len: u8,
        bytes: [u8; MANAGEMENT_HOST_NAME_LEN],
    ) -> Result<Self, ManagementProtocolError> {
        if len as usize > MANAGEMENT_HOST_NAME_LEN
            || !bytes[..len as usize]
                .iter()
                .all(|byte| *byte == b' ' || byte.is_ascii_graphic())
        {
            return Err(ManagementProtocolError::InvalidArgument);
        }
        Ok(Self { len, bytes })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagementHostStatus {
    pub known: bool,
    pub connected: bool,
    pub encrypted: bool,
    pub bonded: bool,
}

impl ManagementHostStatus {
    pub const fn empty() -> Self {
        Self {
            known: false,
            connected: false,
            encrypted: false,
            bonded: false,
        }
    }

    pub const fn bits(self) -> u8 {
        (self.known as u8)
            | ((self.connected as u8) << 1)
            | ((self.encrypted as u8) << 2)
            | ((self.bonded as u8) << 3)
    }

    pub const fn from_bits(bits: u8) -> Self {
        Self {
            known: bits & 0x01 != 0,
            connected: bits & 0x02 != 0,
            encrypted: bits & 0x04 != 0,
            bonded: bits & 0x08 != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementProtocolError {
    InvalidLength,
    UnsupportedVersion,
    UnknownCommand,
    InvalidArgument,
}

const fn option_host_id(host_id: Option<HostId>) -> u8 {
    match host_id {
        Some(host_id) => host_id.0,
        None => 0,
    }
}

const fn decode_optional_host(value: u8) -> Option<HostId> {
    if value == 0 {
        None
    } else {
        Some(HostId(value))
    }
}

fn encode_setting_target(bytes: &mut [u8], target: SettingTarget) {
    match target {
        SettingTarget::Global => {
            bytes[0] = 0;
            bytes[1] = 0;
        }
        SettingTarget::Host(host) => {
            bytes[0] = 1;
            bytes[1] = host.0;
        }
    }
}

fn decode_setting_target(scope: u8, target: u8) -> Result<SettingTarget, ManagementProtocolError> {
    match (scope, target) {
        (0, 0) => Ok(SettingTarget::Global),
        (1, 1..=4) => Ok(SettingTarget::Host(HostId(target))),
        _ => Err(ManagementProtocolError::InvalidArgument),
    }
}

#[cfg(feature = "dual-s3-wired")]
const fn availability_byte(value: OutputTargetAvailability) -> u8 {
    match value {
        OutputTargetAvailability::Ready => 0,
        OutputTargetAvailability::ConnectedNotReady => 1,
        OutputTargetAvailability::Unavailable => 2,
    }
}

#[cfg(feature = "dual-s3-wired")]
fn availability_from_byte(value: u8) -> Result<OutputTargetAvailability, ManagementProtocolError> {
    match value {
        0 => Ok(OutputTargetAvailability::Ready),
        1 => Ok(OutputTargetAvailability::ConnectedNotReady),
        2 => Ok(OutputTargetAvailability::Unavailable),
        _ => Err(ManagementProtocolError::InvalidArgument),
    }
}

#[cfg(feature = "dual-s3-wired")]
fn decode_bool(value: u8) -> Result<bool, ManagementProtocolError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ManagementProtocolError::InvalidArgument),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_request_round_trips_in_one_att_packet() {
        let name = ManagementHostName::from_ascii("Work laptop").unwrap();
        for command in [
            ManagementCommand::GetStatus,
            ManagementCommand::SelectHost(HostId(2)),
            ManagementCommand::StartPairing(HostId(3)),
            ManagementCommand::ForgetHost(HostId(4)),
            ManagementCommand::GetHostInfo(HostId(1)),
            ManagementCommand::SetHostName {
                host_id: HostId(2),
                name,
            },
            ManagementCommand::CancelPairing,
            ManagementCommand::GetUsbDevice {
                index: 1,
                name_offset: 5,
            },
            ManagementCommand::GetDiagnostics,
            ManagementCommand::GetHostTiming(HostId(4)),
            ManagementCommand::GetHistory { index: 3 },
            ManagementCommand::GetSchema,
            ManagementCommand::GetSetting {
                id: SettingId::AutoReconnect,
                target: SettingTarget::Global,
            },
            ManagementCommand::SetSetting {
                id: SettingId::MouseSensitivityPercent,
                target: SettingTarget::Host(HostId(2)),
                value: 175,
            },
        ] {
            let request = ManagementRequest {
                request_id: 7,
                command,
            };
            assert_eq!(ManagementRequest::decode(&request.encode()), Ok(request));
        }
        assert_eq!(MANAGEMENT_REQUEST_LEN, 20);
        assert_eq!(MANAGEMENT_RESPONSE_LEN, 20);
    }

    #[test]
    fn status_response_round_trips_usb_and_host_state() {
        let mut status = ManagementStatus::empty(4);
        status.active_host = Some(HostId(2));
        status.usb = ManagementUsbStatus {
            device_count: 2,
            interface_count: 3,
            keyboard_count: 1,
        };
        status.hosts[1] = ManagementHostStatus {
            known: true,
            connected: true,
            encrypted: true,
            bonded: true,
        };
        let response = ManagementResponse {
            request_id: 9,
            result: ManagementResult::Ok,
            payload: ManagementResponsePayload::Status(status),
        };
        assert_eq!(ManagementResponse::decode(&response.encode()), Ok(response));
    }

    #[cfg(feature = "dual-s3-wired")]
    #[test]
    fn output_target_commands_and_status_round_trip() {
        for command in [
            ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Wired),
            ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Ble(HostId(4))),
            ManagementCommand::GetOutputTargetStatus,
        ] {
            let request = ManagementRequest {
                request_id: 19,
                command,
            };
            assert_eq!(ManagementRequest::decode(&request.encode()), Ok(request));
        }

        let status = ManagementOutputTargetStatus {
            selected: ManagementOutputTarget::Wired,
            active: Some(ManagementOutputTarget::Wired),
            availability: OutputTargetAvailability::Ready,
            wired_ready: true,
            ready_ble_mask: 0b0101,
            effective_presentation: ManagementUsbPresentationKind::Fallback,
            mirror_configured: false,
            operation_id: 0x1234_5678,
        };
        let response = ManagementResponse {
            request_id: 19,
            result: ManagementResult::Ok,
            payload: ManagementResponsePayload::OutputTargetStatus(status),
        };
        assert_eq!(ManagementResponse::decode(&response.encode()), Ok(response));

        let mut invalid = ManagementRequest {
            request_id: 1,
            command: ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Wired),
        }
        .encode();
        invalid[5] = 1;
        assert_eq!(
            ManagementRequest::decode(&invalid),
            Err(ManagementProtocolError::InvalidArgument)
        );
    }

    #[test]
    fn host_info_response_round_trips_name() {
        let response = ManagementResponse {
            request_id: 3,
            result: ManagementResult::Ok,
            payload: ManagementResponsePayload::HostInfo(ManagementHostInfo {
                host_id: HostId(3),
                status: ManagementHostStatus::empty(),
                name: ManagementHostName::from_ascii("Gaming PC").unwrap(),
                name_source: 2,
            }),
        };
        assert_eq!(ManagementResponse::decode(&response.encode()), Ok(response));
    }

    #[test]
    fn malformed_lengths_versions_and_names_are_rejected() {
        assert_eq!(
            ManagementRequest::decode(&[0; 19]),
            Err(ManagementProtocolError::InvalidLength)
        );
        let mut request = ManagementRequest {
            request_id: 1,
            command: ManagementCommand::GetStatus,
        }
        .encode();
        request[0] = MANAGEMENT_PROTOCOL_VERSION.wrapping_add(1);
        assert_eq!(
            ManagementRequest::decode(&request),
            Err(ManagementProtocolError::UnsupportedVersion)
        );
        assert!(ManagementHostName::from_ascii("name that is too long").is_err());
        assert!(ManagementHostName::from_ascii("bad\nname").is_err());
    }

    #[test]
    fn extended_payloads_round_trip_in_default_att_packet() {
        let payloads = [
            ManagementResponsePayload::UsbDevice(ManagementUsbDevice {
                index: 1,
                device_id: 7,
                flags: 3,
                vendor_id: 0x1234,
                product_id: 0xabcd,
                name_len: 8,
                name_offset: 5,
                name_chunk_len: 3,
                name_chunk: *b"KBD\0\0",
            }),
            ManagementResponsePayload::Diagnostics(ManagementDiagnostics {
                uptime_seconds: 123,
                reset_reason: 15,
                brownout_count: 1,
                ble_disconnect_count: 2,
                ble_notify_failure_count: 3,
                usb_error_count: 4,
                flash_write_count: 5,
                flash_failure_count: 6,
            }),
            ManagementResponsePayload::History(ManagementHistoryEvent {
                kind: 2,
                sequence: 9,
                timestamp_seconds: 50,
                subject: 3,
                detail: 0x13,
                vendor_id: 0x1111,
                product_id: 0x2222,
            }),
            ManagementResponsePayload::Schema(ManagementSchema {
                version: 1,
                setting_count: 15,
                history_capacity: 16,
                usb_capacity: 8,
                hash: 0x12345678,
                firmware_major: 0,
                firmware_minor: 1,
                firmware_patch: 0,
            }),
            ManagementResponsePayload::Setting(ManagementSetting {
                id: SettingId::AutoReconnect,
                target: SettingTarget::Global,
                value: 1,
            }),
            ManagementResponsePayload::HostTiming(ManagementHostTiming {
                host_id: HostId(2),
                last_connected_seconds: 11,
                last_disconnected_seconds: 22,
                last_disconnect_reason: 0x13,
            }),
        ];
        for payload in payloads {
            let response = ManagementResponse {
                request_id: 42,
                result: ManagementResult::Ok,
                payload,
            };
            assert_eq!(ManagementResponse::decode(&response.encode()), Ok(response));
        }
    }
}
