use crate::ids::{DeviceId, InterfaceId};
use crate::input::MouseMovement;

pub const MAX_HID_INTERFACES: usize = 8;
pub const MAX_HID_REPORT_DESCRIPTOR_SIZE: usize = 1024;
pub const MAX_HID_REPORT_SIZE: usize = 256;
pub const MAX_BRIDGE_MESSAGE_SIZE: usize = 1 + 9 + MAX_HID_REPORT_DESCRIPTOR_SIZE;

const MESSAGE_HELLO: u8 = 0x01;
const MESSAGE_INTERFACE_DESCRIPTOR: u8 = 0x02;
const MESSAGE_INPUT_REPORT: u8 = 0x03;
const MESSAGE_SET_REPORT: u8 = 0x04;
const MESSAGE_GET_REPORT: u8 = 0x05;
const MESSAGE_GET_REPORT_RESPONSE: u8 = 0x06;
const MESSAGE_INTERFACE_REMOVED: u8 = 0x07;
const MESSAGE_RELEASE_ALL: u8 = 0x08;
const MESSAGE_E2E_TIMESTAMP: u8 = 0x09;
const MESSAGE_DESCRIPTOR_SNAPSHOT_END: u8 = 0x0a;
const MESSAGE_E2E_BRIDGE_TIMESTAMP: u8 = 0x0b;
const MESSAGE_ENTER_DOWNLOAD_MODE: u8 = 0x0f;
const MESSAGE_INPUT_SNAPSHOT: u8 = 0x11;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum BridgeRole {
    UsbHost = 1,
    UsbDevice = 2,
}

impl BridgeRole {
    fn decode(value: u8) -> Result<Self, BridgeMessageError> {
        match value {
            1 => Ok(Self::UsbHost),
            2 => Ok(Self::UsbDevice),
            _ => Err(BridgeMessageError::InvalidRole),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum HidReportType {
    Input = 1,
    Output = 2,
    Feature = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum InputLane {
    Motion = 1,
    Critical = 2,
}

impl InputLane {
    fn decode(value: u8) -> Result<Self, BridgeMessageError> {
        match value {
            1 => Ok(Self::Motion),
            2 => Ok(Self::Critical),
            _ => Err(BridgeMessageError::InvalidInputLane),
        }
    }
}

impl HidReportType {
    fn decode(value: u8) -> Result<Self, BridgeMessageError> {
        match value {
            1 => Ok(Self::Input),
            2 => Ok(Self::Output),
            3 => Ok(Self::Feature),
            _ => Err(BridgeMessageError::InvalidReportType),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InterfaceDescriptor<'a> {
    pub device_id: DeviceId,
    pub interface_id: InterfaceId,
    pub interface_index: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub descriptor: &'a [u8],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MotionCumulative {
    pub x: i32,
    pub y: i32,
    pub wheel: i32,
    pub pan: i32,
}

impl MotionCumulative {
    pub const fn zero() -> Self {
        Self {
            x: 0,
            y: 0,
            wheel: 0,
            pan: 0,
        }
    }

    pub const fn from_movement(movement: MouseMovement) -> Self {
        Self {
            x: movement.x as i32,
            y: movement.y as i32,
            wheel: movement.wheel as i32,
            pan: movement.pan as i32,
        }
    }

    pub fn delta_from(self, previous: Option<Self>) -> MouseMovement {
        let previous = match previous {
            Some(previous) => previous,
            None => Self::zero(),
        };
        MouseMovement {
            x: self
                .x
                .wrapping_sub(previous.x)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            y: self
                .y
                .wrapping_sub(previous.y)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            wheel: self
                .wheel
                .wrapping_sub(previous.wheel)
                .clamp(i8::MIN as i32, i8::MAX as i32) as i8,
            pan: self
                .pan
                .wrapping_sub(previous.pan)
                .clamp(i8::MIN as i32, i8::MAX as i32) as i8,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeMessage<'a> {
    Hello {
        role: BridgeRole,
        capabilities: u16,
        session_id: u32,
        /// The peer session this Hello acknowledges. Zero means that the
        /// sender has not received the peer's current session yet.
        peer_session_id: u32,
        next_critical_sequence: u32,
        next_motion_sequence: u32,
    },
    InterfaceDescriptor(InterfaceDescriptor<'a>),
    InputReport {
        device_id: DeviceId,
        interface_id: InterfaceId,
        /// Sequence number in the selected input lane. Motion and critical
        /// reports intentionally use independent sequence spaces so that
        /// coalescing motion cannot look like critical packet loss.
        sequence: u32,
        lane: InputLane,
        /// Hardware-E2E correlation id. Zero means this is a normal runtime
        /// report and does not carry test instrumentation metadata.
        e2e_sequence: u32,
        motion: MotionCumulative,
        report: &'a [u8],
    },
    /// A chronological suffix of complete HID state transitions. Records are
    /// repeated in newer snapshots so a lost broadcast is recovered without
    /// waiting for, or retransmitting, a particular radio packet.
    InputSnapshot {
        record_count: u8,
        records: &'a [u8],
    },
    SetReport {
        device_id: DeviceId,
        interface_id: InterfaceId,
        report_type: HidReportType,
        report_id: u8,
        report: &'a [u8],
    },
    GetReport {
        device_id: DeviceId,
        interface_id: InterfaceId,
        report_type: HidReportType,
        report_id: u8,
        requested_len: u16,
        request_id: u16,
    },
    GetReportResponse {
        device_id: DeviceId,
        interface_id: InterfaceId,
        report_type: HidReportType,
        report_id: u8,
        request_id: u16,
        report: &'a [u8],
    },
    InterfaceRemoved {
        device_id: DeviceId,
        interface_id: InterfaceId,
    },
    ReleaseAll,
    EnterDownloadMode,
    E2eTimestamp {
        sequence: u32,
        ingress_us: u64,
    },
    E2eBridgeTimestamp {
        sequence: u32,
        radio_rx_us: u64,
        reassembled_us: u64,
        hid_write_us: u64,
    },
    DescriptorSnapshotEnd {
        interface_count: u8,
        generation: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BridgeMessageError {
    BufferTooSmall,
    MessageTooLarge,
    DescriptorTooLarge,
    ReportTooLarge,
    InvalidLength,
    UnknownMessage,
    InvalidRole,
    InvalidReportType,
    InvalidInputLane,
    InvalidSnapshot,
}

impl<'a> BridgeMessage<'a> {
    pub fn encode(self, out: &mut [u8]) -> Result<usize, BridgeMessageError> {
        let required = self.encoded_len()?;
        if out.len() < required {
            return Err(BridgeMessageError::BufferTooSmall);
        }

        match self {
            Self::Hello {
                role,
                capabilities,
                session_id,
                peer_session_id,
                next_critical_sequence,
                next_motion_sequence,
            } => {
                out[0] = MESSAGE_HELLO;
                out[1] = role as u8;
                out[2..4].copy_from_slice(&capabilities.to_le_bytes());
                out[4..8].copy_from_slice(&session_id.to_le_bytes());
                out[8..12].copy_from_slice(&peer_session_id.to_le_bytes());
                out[12..16].copy_from_slice(&next_critical_sequence.to_le_bytes());
                out[16..20].copy_from_slice(&next_motion_sequence.to_le_bytes());
            }
            Self::InterfaceDescriptor(descriptor) => {
                out[0] = MESSAGE_INTERFACE_DESCRIPTOR;
                out[1] = descriptor.device_id.0;
                out[2] = descriptor.interface_id.0;
                out[3] = descriptor.interface_index;
                out[4..6].copy_from_slice(&descriptor.vendor_id.to_le_bytes());
                out[6..8].copy_from_slice(&descriptor.product_id.to_le_bytes());
                out[8..10].copy_from_slice(&(descriptor.descriptor.len() as u16).to_le_bytes());
                out[10..required].copy_from_slice(descriptor.descriptor);
            }
            Self::InputReport {
                device_id,
                interface_id,
                sequence,
                lane,
                e2e_sequence,
                motion,
                report,
            } => {
                out[0] = MESSAGE_INPUT_REPORT;
                out[1] = device_id.0;
                out[2] = interface_id.0;
                out[3] = lane as u8;
                out[4..8].copy_from_slice(&sequence.to_le_bytes());
                out[8..12].copy_from_slice(&e2e_sequence.to_le_bytes());
                if lane == InputLane::Motion {
                    out[12..16].copy_from_slice(&motion.x.to_le_bytes());
                    out[16..20].copy_from_slice(&motion.y.to_le_bytes());
                    out[20..24].copy_from_slice(&motion.wheel.to_le_bytes());
                    out[24..28].copy_from_slice(&motion.pan.to_le_bytes());
                    out[28..30].copy_from_slice(&(report.len() as u16).to_le_bytes());
                    out[30..required].copy_from_slice(report);
                } else {
                    out[12..14].copy_from_slice(&(report.len() as u16).to_le_bytes());
                    out[14..required].copy_from_slice(report);
                }
            }
            Self::InputSnapshot {
                record_count,
                records,
            } => {
                out[0] = MESSAGE_INPUT_SNAPSHOT;
                out[1] = record_count;
                out[2..required].copy_from_slice(records);
            }
            Self::SetReport {
                device_id,
                interface_id,
                report_type,
                report_id,
                report,
            } => {
                out[0] = MESSAGE_SET_REPORT;
                out[1] = device_id.0;
                out[2] = interface_id.0;
                out[3] = report_type as u8;
                out[4] = report_id;
                out[5..7].copy_from_slice(&(report.len() as u16).to_le_bytes());
                out[7..required].copy_from_slice(report);
            }
            Self::GetReport {
                device_id,
                interface_id,
                report_type,
                report_id,
                requested_len,
                request_id,
            } => {
                out[0] = MESSAGE_GET_REPORT;
                out[1] = device_id.0;
                out[2] = interface_id.0;
                out[3] = report_type as u8;
                out[4] = report_id;
                out[5..7].copy_from_slice(&requested_len.to_le_bytes());
                out[7..9].copy_from_slice(&request_id.to_le_bytes());
            }
            Self::GetReportResponse {
                device_id,
                interface_id,
                report_type,
                report_id,
                request_id,
                report,
            } => {
                out[0] = MESSAGE_GET_REPORT_RESPONSE;
                out[1] = device_id.0;
                out[2] = interface_id.0;
                out[3] = report_type as u8;
                out[4] = report_id;
                out[5..7].copy_from_slice(&request_id.to_le_bytes());
                out[7..9].copy_from_slice(&(report.len() as u16).to_le_bytes());
                out[9..required].copy_from_slice(report);
            }
            Self::InterfaceRemoved {
                device_id,
                interface_id,
            } => {
                out[0] = MESSAGE_INTERFACE_REMOVED;
                out[1] = device_id.0;
                out[2] = interface_id.0;
            }
            Self::ReleaseAll => out[0] = MESSAGE_RELEASE_ALL,
            Self::EnterDownloadMode => out[0] = MESSAGE_ENTER_DOWNLOAD_MODE,
            Self::E2eTimestamp {
                sequence,
                ingress_us,
            } => {
                out[0] = MESSAGE_E2E_TIMESTAMP;
                out[1..5].copy_from_slice(&sequence.to_le_bytes());
                out[5..13].copy_from_slice(&ingress_us.to_le_bytes());
            }
            Self::E2eBridgeTimestamp {
                sequence,
                radio_rx_us,
                reassembled_us,
                hid_write_us,
            } => {
                out[0] = MESSAGE_E2E_BRIDGE_TIMESTAMP;
                out[1..5].copy_from_slice(&sequence.to_le_bytes());
                out[5..13].copy_from_slice(&radio_rx_us.to_le_bytes());
                out[13..21].copy_from_slice(&reassembled_us.to_le_bytes());
                out[21..29].copy_from_slice(&hid_write_us.to_le_bytes());
            }
            Self::DescriptorSnapshotEnd {
                interface_count,
                generation,
            } => {
                out[0] = MESSAGE_DESCRIPTOR_SNAPSHOT_END;
                out[1] = interface_count;
                out[2..6].copy_from_slice(&generation.to_le_bytes());
            }
        }
        Ok(required)
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, BridgeMessageError> {
        let kind = *bytes.first().ok_or(BridgeMessageError::InvalidLength)?;
        match kind {
            MESSAGE_HELLO if bytes.len() == 4 => Ok(Self::Hello {
                role: BridgeRole::decode(bytes[1])?,
                capabilities: u16::from_le_bytes([bytes[2], bytes[3]]),
                session_id: 0,
                peer_session_id: 0,
                next_critical_sequence: 1,
                next_motion_sequence: 1,
            }),
            MESSAGE_HELLO if bytes.len() == 16 => Ok(Self::Hello {
                role: BridgeRole::decode(bytes[1])?,
                capabilities: u16::from_le_bytes([bytes[2], bytes[3]]),
                session_id: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
                peer_session_id: 0,
                next_critical_sequence: u32::from_le_bytes([
                    bytes[8], bytes[9], bytes[10], bytes[11],
                ]),
                next_motion_sequence: u32::from_le_bytes([
                    bytes[12], bytes[13], bytes[14], bytes[15],
                ]),
            }),
            MESSAGE_HELLO if bytes.len() == 20 => Ok(Self::Hello {
                role: BridgeRole::decode(bytes[1])?,
                capabilities: u16::from_le_bytes([bytes[2], bytes[3]]),
                session_id: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
                peer_session_id: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
                next_critical_sequence: u32::from_le_bytes([
                    bytes[12], bytes[13], bytes[14], bytes[15],
                ]),
                next_motion_sequence: u32::from_le_bytes([
                    bytes[16], bytes[17], bytes[18], bytes[19],
                ]),
            }),
            MESSAGE_INTERFACE_DESCRIPTOR if bytes.len() >= 10 => {
                let len = u16::from_le_bytes([bytes[8], bytes[9]]) as usize;
                if len > MAX_HID_REPORT_DESCRIPTOR_SIZE {
                    return Err(BridgeMessageError::DescriptorTooLarge);
                }
                if bytes.len() != 10 + len {
                    return Err(BridgeMessageError::InvalidLength);
                }
                Ok(Self::InterfaceDescriptor(InterfaceDescriptor {
                    device_id: DeviceId(bytes[1]),
                    interface_id: InterfaceId(bytes[2]),
                    interface_index: bytes[3],
                    vendor_id: u16::from_le_bytes([bytes[4], bytes[5]]),
                    product_id: u16::from_le_bytes([bytes[6], bytes[7]]),
                    descriptor: &bytes[10..],
                }))
            }
            MESSAGE_INPUT_REPORT if bytes.len() >= 5 => {
                if bytes.len() < 14 {
                    return Err(BridgeMessageError::InvalidLength);
                }
                let lane = InputLane::decode(bytes[3])?;
                let (motion, report_offset, length_offset) = if lane == InputLane::Motion {
                    if bytes.len() < 30 {
                        return Err(BridgeMessageError::InvalidLength);
                    }
                    (
                        MotionCumulative {
                            x: i32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
                            y: i32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
                            wheel: i32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
                            pan: i32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]),
                        },
                        30,
                        28,
                    )
                } else {
                    (MotionCumulative::zero(), 14, 12)
                };
                let report = checked_payload(bytes, report_offset, length_offset)?;
                Ok(Self::InputReport {
                    device_id: DeviceId(bytes[1]),
                    interface_id: InterfaceId(bytes[2]),
                    lane,
                    sequence: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
                    e2e_sequence: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
                    motion,
                    report,
                })
            }
            MESSAGE_INPUT_SNAPSHOT if bytes.len() >= 2 => {
                let record_count = bytes[1];
                super::snapshot::InputSnapshotRecords::new(record_count, &bytes[2..])
                    .map_err(|_| BridgeMessageError::InvalidSnapshot)?;
                Ok(Self::InputSnapshot {
                    record_count,
                    records: &bytes[2..],
                })
            }
            MESSAGE_SET_REPORT if bytes.len() >= 7 => {
                let report = checked_payload(bytes, 7, 5)?;
                Ok(Self::SetReport {
                    device_id: DeviceId(bytes[1]),
                    interface_id: InterfaceId(bytes[2]),
                    report_type: HidReportType::decode(bytes[3])?,
                    report_id: bytes[4],
                    report,
                })
            }
            MESSAGE_GET_REPORT if bytes.len() == 9 => Ok(Self::GetReport {
                device_id: DeviceId(bytes[1]),
                interface_id: InterfaceId(bytes[2]),
                report_type: HidReportType::decode(bytes[3])?,
                report_id: bytes[4],
                requested_len: u16::from_le_bytes([bytes[5], bytes[6]]),
                request_id: u16::from_le_bytes([bytes[7], bytes[8]]),
            }),
            MESSAGE_GET_REPORT_RESPONSE if bytes.len() >= 9 => {
                let report = checked_payload(bytes, 9, 7)?;
                Ok(Self::GetReportResponse {
                    device_id: DeviceId(bytes[1]),
                    interface_id: InterfaceId(bytes[2]),
                    report_type: HidReportType::decode(bytes[3])?,
                    report_id: bytes[4],
                    request_id: u16::from_le_bytes([bytes[5], bytes[6]]),
                    report,
                })
            }
            MESSAGE_INTERFACE_REMOVED if bytes.len() == 3 => Ok(Self::InterfaceRemoved {
                device_id: DeviceId(bytes[1]),
                interface_id: InterfaceId(bytes[2]),
            }),
            MESSAGE_RELEASE_ALL if bytes.len() == 1 => Ok(Self::ReleaseAll),
            MESSAGE_ENTER_DOWNLOAD_MODE if bytes.len() == 1 => Ok(Self::EnterDownloadMode),
            MESSAGE_E2E_TIMESTAMP if bytes.len() == 13 => Ok(Self::E2eTimestamp {
                sequence: u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
                ingress_us: u64::from_le_bytes([
                    bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
                    bytes[12],
                ]),
            }),
            MESSAGE_E2E_BRIDGE_TIMESTAMP if bytes.len() == 29 => Ok(Self::E2eBridgeTimestamp {
                sequence: u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]),
                radio_rx_us: u64::from_le_bytes([
                    bytes[5], bytes[6], bytes[7], bytes[8], bytes[9], bytes[10], bytes[11],
                    bytes[12],
                ]),
                reassembled_us: u64::from_le_bytes([
                    bytes[13], bytes[14], bytes[15], bytes[16], bytes[17], bytes[18], bytes[19],
                    bytes[20],
                ]),
                hid_write_us: u64::from_le_bytes([
                    bytes[21], bytes[22], bytes[23], bytes[24], bytes[25], bytes[26], bytes[27],
                    bytes[28],
                ]),
            }),
            MESSAGE_DESCRIPTOR_SNAPSHOT_END if bytes.len() == 6 => {
                Ok(Self::DescriptorSnapshotEnd {
                    interface_count: bytes[1],
                    generation: u32::from_le_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]),
                })
            }
            MESSAGE_HELLO
            | MESSAGE_INTERFACE_DESCRIPTOR
            | MESSAGE_INPUT_REPORT
            | MESSAGE_INPUT_SNAPSHOT
            | MESSAGE_SET_REPORT
            | MESSAGE_GET_REPORT
            | MESSAGE_GET_REPORT_RESPONSE
            | MESSAGE_INTERFACE_REMOVED
            | MESSAGE_RELEASE_ALL
            | MESSAGE_ENTER_DOWNLOAD_MODE
            | MESSAGE_E2E_TIMESTAMP => Err(BridgeMessageError::InvalidLength),
            MESSAGE_E2E_BRIDGE_TIMESTAMP => Err(BridgeMessageError::InvalidLength),
            MESSAGE_DESCRIPTOR_SNAPSHOT_END => Err(BridgeMessageError::InvalidLength),
            _ => Err(BridgeMessageError::UnknownMessage),
        }
    }

    fn encoded_len(self) -> Result<usize, BridgeMessageError> {
        let len = match self {
            Self::Hello { .. } => 20,
            Self::InterfaceDescriptor(value) => {
                if value.descriptor.len() > MAX_HID_REPORT_DESCRIPTOR_SIZE {
                    return Err(BridgeMessageError::DescriptorTooLarge);
                }
                10 + value.descriptor.len()
            }
            Self::InputReport { lane, report, .. } => {
                check_report_len(report.len())?;
                if lane == InputLane::Motion {
                    30 + report.len()
                } else {
                    14 + report.len()
                }
            }
            Self::InputSnapshot {
                record_count,
                records,
            } => {
                super::snapshot::InputSnapshotRecords::new(record_count, records)
                    .map_err(|_| BridgeMessageError::InvalidSnapshot)?;
                2 + records.len()
            }
            Self::GetReportResponse { report, .. } => {
                check_report_len(report.len())?;
                9 + report.len()
            }
            Self::SetReport { report, .. } => {
                check_report_len(report.len())?;
                7 + report.len()
            }
            Self::GetReport { .. } => 9,
            Self::InterfaceRemoved { .. } => 3,
            Self::ReleaseAll => 1,
            Self::EnterDownloadMode => 1,
            Self::E2eTimestamp { .. } => 13,
            Self::E2eBridgeTimestamp { .. } => 29,
            Self::DescriptorSnapshotEnd { .. } => 6,
        };
        if len > MAX_BRIDGE_MESSAGE_SIZE {
            return Err(BridgeMessageError::MessageTooLarge);
        }
        Ok(len)
    }
}

fn checked_payload(
    bytes: &[u8],
    payload_offset: usize,
    length_offset: usize,
) -> Result<&[u8], BridgeMessageError> {
    let len = u16::from_le_bytes([bytes[length_offset], bytes[length_offset + 1]]) as usize;
    check_report_len(len)?;
    if bytes.len() != payload_offset + len {
        return Err(BridgeMessageError::InvalidLength);
    }
    Ok(&bytes[payload_offset..])
}

fn check_report_len(len: usize) -> Result<(), BridgeMessageError> {
    if len > MAX_HID_REPORT_SIZE {
        Err(BridgeMessageError::ReportTooLarge)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(message: BridgeMessage<'_>) {
        let mut bytes = [0; MAX_BRIDGE_MESSAGE_SIZE];
        let len = message.encode(&mut bytes).unwrap();
        assert_eq!(BridgeMessage::decode(&bytes[..len]), Ok(message));
    }

    #[test]
    fn all_bidirectional_hid_messages_round_trip() {
        round_trip(BridgeMessage::Hello {
            role: BridgeRole::UsbHost,
            capabilities: 0x1234,
            session_id: 0x5566_7788,
            peer_session_id: 0x1122_3344,
            next_critical_sequence: 42,
            next_motion_sequence: 99,
        });
        round_trip(BridgeMessage::InterfaceDescriptor(InterfaceDescriptor {
            device_id: DeviceId(1),
            interface_id: InterfaceId(2),
            interface_index: 3,
            vendor_id: 0x046d,
            product_id: 0xc539,
            descriptor: &[0x06, 0x00, 0xff, 0x09, 0x01],
        }));
        round_trip(BridgeMessage::InputReport {
            device_id: DeviceId(1),
            interface_id: InterfaceId(2),
            lane: InputLane::Critical,
            sequence: 7,
            e2e_sequence: 0x1122_3344,
            motion: MotionCumulative::zero(),
            report: &[7, 1, 2, 3],
        });
        let records = [
            2, // critical lane
            1, 2, // device/interface
            7, 0, 0, 0, // sequence
            9, 0, 0, 0, // E2E sequence
            2, 0, // report length
            1, 4, // report
        ];
        round_trip(BridgeMessage::InputSnapshot {
            record_count: 1,
            records: &records,
        });
        round_trip(BridgeMessage::InputReport {
            device_id: DeviceId(1),
            interface_id: InterfaceId(2),
            lane: InputLane::Motion,
            sequence: 8,
            e2e_sequence: 0x5566_7788,
            motion: MotionCumulative {
                x: 1000,
                y: -2000,
                wheel: 3,
                pan: -4,
            },
            report: &[7, 1, 2, 3],
        });
        for report_type in [HidReportType::Output, HidReportType::Feature] {
            round_trip(BridgeMessage::SetReport {
                device_id: DeviceId(1),
                interface_id: InterfaceId(2),
                report_type,
                report_id: 7,
                report: &[7, 0xaa, 0x55],
            });
            round_trip(BridgeMessage::GetReport {
                device_id: DeviceId(1),
                interface_id: InterfaceId(2),
                report_type,
                report_id: 7,
                requested_len: 64,
                request_id: 99,
            });
        }
        round_trip(BridgeMessage::GetReportResponse {
            device_id: DeviceId(1),
            interface_id: InterfaceId(2),
            report_type: HidReportType::Feature,
            report_id: 7,
            request_id: 99,
            report: &[7, 0x42],
        });
        round_trip(BridgeMessage::ReleaseAll);
        round_trip(BridgeMessage::EnterDownloadMode);
        round_trip(BridgeMessage::DescriptorSnapshotEnd {
            interface_count: 3,
            generation: 7,
        });
        round_trip(BridgeMessage::E2eBridgeTimestamp {
            sequence: 11,
            radio_rx_us: 22,
            reassembled_us: 33,
            hid_write_us: 44,
        });
    }

    #[test]
    fn descriptor_and_report_limits_are_explicit() {
        let descriptor = [0; MAX_HID_REPORT_DESCRIPTOR_SIZE + 1];
        let report = [0; MAX_HID_REPORT_SIZE + 1];
        let mut out = [0; MAX_BRIDGE_MESSAGE_SIZE + 2];
        assert_eq!(
            BridgeMessage::InterfaceDescriptor(InterfaceDescriptor {
                device_id: DeviceId(1),
                interface_id: InterfaceId(1),
                interface_index: 0,
                vendor_id: 0,
                product_id: 0,
                descriptor: &descriptor,
            })
            .encode(&mut out),
            Err(BridgeMessageError::DescriptorTooLarge)
        );
        assert_eq!(
            BridgeMessage::InputReport {
                device_id: DeviceId(1),
                interface_id: InterfaceId(1),
                lane: InputLane::Critical,
                sequence: 0,
                e2e_sequence: 0,
                motion: MotionCumulative::zero(),
                report: &report,
            }
            .encode(&mut out),
            Err(BridgeMessageError::ReportTooLarge)
        );
    }

    #[test]
    fn cumulative_motion_recovers_a_dropped_intermediate_report() {
        let first = MotionCumulative {
            x: 7,
            y: -2,
            wheel: 1,
            pan: 0,
        };
        let after_dropped_packet = MotionCumulative {
            x: 21,
            y: -6,
            wheel: 3,
            pan: -1,
        };

        assert_eq!(first.delta_from(None).x, 7);
        assert_eq!(
            after_dropped_packet.delta_from(Some(first)),
            MouseMovement {
                x: 14,
                y: -4,
                wheel: 2,
                pan: -1,
            }
        );
    }
}
