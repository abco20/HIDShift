use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::Instant;
use esp_hal::peripherals::{GPIO44, UART0};
use esp_hal::uart::{Config, Uart};
use hidshift::management::ManagementDestination;
use hidshift::runtime::RUNTIME_INPUT_QUEUE_CAPACITY;
use hidshift::runtime::message::RuntimeInputMessage;

#[cfg(feature = "hardware-e2e")]
use hidshift::e2e::{E2eCommand, E2ePacket};
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
use hidshift::e2e_mirror::{
    MirrorE2ePacket, MirrorRawInjectionReceiver, OPCODE_CLEAR_CANDIDATES, OPCODE_HELLO,
    OPCODE_INJECT_ENDPOINT_IN, OPCODE_REGISTER_BEGIN, OPCODE_REGISTER_CHUNK,
    OPCODE_REGISTER_COMMIT, OPCODE_SET_CONTROL_RESPONSE,
};
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
use hidshift::interchip::{
    ControlStatus, MirrorControlResponse, ProfileBegin, ProfileChunk, ProfileResultStatus,
    ProfileTransferEncoder, ProfileTransferReceiver, RawEndpointReport,
};
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
use hidshift::mirror::HSMI_MAX_SIZE;
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
use hidshift::output_target::MirrorCandidateId;
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
use hidshift::runtime::{DeviceTaskCommand, RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY};
#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
use static_cell::StaticCell;

#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
static MIRROR_E2E_STAGING: StaticCell<[u8; HSMI_MAX_SIZE]> = StaticCell::new();

#[cfg(feature = "hardware-e2e")]
const SERIAL_LINE_CAPACITY: usize = 160;
#[cfg(not(feature = "hardware-e2e"))]
const SERIAL_LINE_CAPACITY: usize = 64;

#[embassy_executor::task]
pub async fn serial_management_task(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))] device_sender: Sender<
        'static,
        CriticalSectionRawMutex,
        DeviceTaskCommand,
        RUNTIME_DEVICE_COMMAND_QUEUE_CAPACITY,
    >,
    uart: UART0<'static>,
    rx: GPIO44<'static>,
    _boot_session_id: u32,
) {
    let Ok(uart) = Uart::new(uart, Config::default()) else {
        log::error!("firmware: management UART init failed");
        return;
    };
    let mut uart = uart.with_rx(rx).into_async();
    let mut line = [0u8; SERIAL_LINE_CAPACITY];
    let mut line_len = 0usize;
    let mut byte = [0u8; 1];

    log::info!("firmware: wired management ready on UART0 RX GPIO44");
    #[cfg(feature = "hardware-e2e")]
    log::info!("@HIDSHIFT-E2E:READY,1");
    #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
    let mut mirror_receiver =
        ProfileTransferReceiver::new(MIRROR_E2E_STAGING.init([0; HSMI_MAX_SIZE]));
    #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
    let mut raw_injection_receiver = MirrorRawInjectionReceiver::new();
    loop {
        match uart.read_async(&mut byte).await {
            Ok(1) if byte[0] == b'\n' || byte[0] == b'\r' => {
                if let Some(request) =
                    crate::wired_management::decode_request_line(&line[..line_len])
                {
                    sender
                        .send(RuntimeInputMessage::ManagementRequest {
                            destination: ManagementDestination::Wired,
                            request,
                            now_ms: Instant::now().as_millis(),
                        })
                        .await;
                }
                #[cfg(feature = "hardware-e2e")]
                if let Ok(packet) = E2ePacket::decode_line(&line[..line_len]) {
                    let sequence = packet.sequence;
                    let acknowledge = packet.requests_acknowledgement();
                    let ingress_us = Instant::now().as_micros();
                    if matches!(packet.command, E2eCommand::Hello) {
                        log::info!(
                            "@HIDSHIFT-BRIDGE:CLOCK,{},{},{},{}",
                            sequence,
                            _boot_session_id,
                            device_session_id(),
                            Instant::now().as_micros()
                        );
                    }
                    if packet.carries_input() {
                        crate::e2e_telemetry::record_ingress(sequence, ingress_us);
                    }
                    if let E2eCommand::ReadTimestamp { .. } = packet.command {
                        let snapshot = crate::e2e_telemetry::snapshot();
                        log::info!(
                            "@HIDSHIFT-E2E:STAMP,{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                            sequence,
                            snapshot.sequence,
                            snapshot.ingress_us,
                            snapshot.runtime_us,
                            snapshot.runtime_dispatch_us,
                            snapshot.ble_queued_us,
                            snapshot.ble_receive_us,
                            snapshot.notify_start_us,
                            snapshot.notify_done_us,
                            snapshot.input_count,
                            snapshot.ble_queued_count,
                            snapshot.notify_done_count,
                            u8::from(snapshot.ble_connected),
                            snapshot.ble_connection_interval_us,
                            snapshot.ble_peripheral_latency,
                            snapshot.ble_supervision_timeout_ms,
                            snapshot.ble_tx_phy,
                            snapshot.ble_rx_phy,
                            snapshot.ble_parameter_updates,
                            snapshot.ble_phy_updates,
                            snapshot.hci_submit_us,
                            snapshot.hci_dequeue_us,
                            snapshot.hci_credit_us
                        );
                    }
                    match packet.input_frames() {
                        Ok(frames) => {
                            for frame in frames.into_iter().flatten() {
                                sender
                                    .send(RuntimeInputMessage::BridgeEvent(
                                        hidshift::BridgeEvent::InputFrame(frame),
                                    ))
                                    .await;
                            }
                            if acknowledge {
                                log::info!(
                                    "@HIDSHIFT-E2E:QUEUED,{},{}",
                                    sequence,
                                    Instant::now().as_micros()
                                );
                            }
                        }
                        Err(error) => {
                            log::warn!("@HIDSHIFT-E2E:ERROR,{},payload,{:?}", sequence, error)
                        }
                    }
                }
                #[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
                if let Ok(packet) = MirrorE2ePacket::decode_line(&line[..line_len]) {
                    match packet.opcode {
                        OPCODE_HELLO => {
                            log::info!("@HIDSHIFT-MIRROR:READY,{},1", packet.sequence);
                        }
                        OPCODE_REGISTER_BEGIN if packet.payload().len() == 8 => {
                            mirror_receiver.cancel();
                            mirror_receiver.clear_committed();
                            let begin = ProfileBegin {
                                transfer_id: packet.transfer_id,
                                total_length: packet.offset,
                                crc32: read_u32(&packet.payload()[..4]),
                                profile_hash: read_u32(&packet.payload()[4..8]),
                            };
                            match mirror_receiver.begin(begin) {
                                Ok(()) => log::info!(
                                    "@HIDSHIFT-MIRROR:BEGIN,{},{}",
                                    packet.sequence,
                                    packet.transfer_id
                                ),
                                Err(error) => log::warn!(
                                    "@HIDSHIFT-MIRROR:ERROR,{},begin,{:?}",
                                    packet.sequence,
                                    error
                                ),
                            }
                        }
                        OPCODE_REGISTER_CHUNK => {
                            match mirror_receiver.chunk(ProfileChunk {
                                transfer_id: packet.transfer_id,
                                offset: packet.offset,
                                data: packet.payload(),
                            }) {
                                Ok(_) => log::info!(
                                    "@HIDSHIFT-MIRROR:CHUNK,{},{}",
                                    packet.sequence,
                                    packet.offset
                                ),
                                Err(error) => log::warn!(
                                    "@HIDSHIFT-MIRROR:ERROR,{},chunk,{:?}",
                                    packet.sequence,
                                    error
                                ),
                            }
                        }
                        OPCODE_REGISTER_COMMIT => {
                            let result = mirror_receiver.commit(packet.transfer_id);
                            if result.status == ProfileResultStatus::Accepted
                                && let Some((metadata, image)) = mirror_receiver.committed()
                            {
                                if let Ok(transfer) = ProfileTransferEncoder::new(
                                    metadata.transfer_id,
                                    metadata.profile_hash,
                                    image,
                                ) {
                                    for command in transfer {
                                        device_sender.send(command.into()).await;
                                    }
                                    sender
                                        .send(RuntimeInputMessage::MirrorCandidateRegistered {
                                            candidate: MirrorCandidateId(0),
                                            stable_id: hidshift::MirrorStableId::synthetic(
                                                metadata.profile_hash,
                                            ),
                                            profile_hash: Some(metadata.profile_hash),
                                            synthetic: true,
                                            source_device: None,
                                        })
                                        .await;
                                    log::info!(
                                        "@HIDSHIFT-MIRROR:REGISTERED,{},{},{}",
                                        packet.sequence,
                                        metadata.profile_hash,
                                        metadata.length
                                    );
                                }
                            } else {
                                log::warn!(
                                    "@HIDSHIFT-MIRROR:ERROR,{},commit,{},{}",
                                    packet.sequence,
                                    result.status as u8,
                                    result.reject_reason
                                );
                            }
                        }
                        OPCODE_CLEAR_CANDIDATES => {
                            mirror_receiver.cancel();
                            mirror_receiver.clear_committed();
                            sender
                                .send(RuntimeInputMessage::MirrorCandidateRegistered {
                                    candidate: MirrorCandidateId(0),
                                    stable_id: hidshift::MirrorStableId::synthetic(0),
                                    profile_hash: None,
                                    synthetic: true,
                                    source_device: None,
                                })
                                .await;
                            log::info!("@HIDSHIFT-MIRROR:CLEARED,{}", packet.sequence);
                        }
                        OPCODE_INJECT_ENDPOINT_IN => match raw_injection_receiver.push(&packet) {
                            Ok(Some(injection)) => {
                                let report = RawEndpointReport::new(
                                    injection.endpoint_address,
                                    packet.sequence as u16,
                                    injection.data(),
                                );
                                let Ok(report) = report else {
                                    log::warn!(
                                        "@HIDSHIFT-MIRROR:ERROR,{},inject,report",
                                        packet.sequence
                                    );
                                    line_len = 0;
                                    continue;
                                };
                                device_sender
                                    .send(DeviceTaskCommand::RawEndpointIn(report))
                                    .await;
                                log::info!(
                                    "@HIDSHIFT-MIRROR:INJECTED,{},{:02x},{}",
                                    packet.sequence,
                                    injection.endpoint_address,
                                    injection.data().len()
                                );
                            }
                            Ok(None) => {}
                            Err(error) => log::warn!(
                                "@HIDSHIFT-MIRROR:ERROR,{},inject,{:?}",
                                packet.sequence,
                                error
                            ),
                        },
                        OPCODE_SET_CONTROL_RESPONSE if !packet.payload().is_empty() => {
                            let status = match packet.payload()[0] {
                                0 => Some(ControlStatus::Success),
                                1 => Some(ControlStatus::Stall),
                                2 => Some(ControlStatus::Timeout),
                                3 => Some(ControlStatus::Disconnected),
                                4 => Some(ControlStatus::Unsupported),
                                _ => None,
                            };
                            match status.and_then(|status| {
                                MirrorControlResponse::new(0, status, &packet.payload()[1..]).ok()
                            }) {
                                Some(response) => {
                                    sender
                                        .send(RuntimeInputMessage::SyntheticMirrorControlResponse(
                                            response,
                                        ))
                                        .await;
                                    log::info!(
                                        "@HIDSHIFT-MIRROR:CONTROL_RESPONSE_SET,{},{}",
                                        packet.sequence,
                                        packet.payload().len() - 1
                                    );
                                }
                                None => log::warn!(
                                    "@HIDSHIFT-MIRROR:ERROR,{},control-response",
                                    packet.sequence
                                ),
                            }
                        }
                        _ => log::warn!(
                            "@HIDSHIFT-MIRROR:ERROR,{},opcode,{}",
                            packet.sequence,
                            packet.opcode
                        ),
                    }
                }
                line_len = 0;
            }
            Ok(1) => {
                if line_len < line.len() {
                    line[line_len] = byte[0];
                    line_len += 1;
                } else {
                    line_len = 0;
                }
            }
            Ok(_) => {}
            Err(error) => log::warn!("firmware: management UART read failed: {:?}", error),
        }
    }
}

#[cfg(all(feature = "hardware-e2e", feature = "dual-s3-wired"))]
fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(feature = "hardware-e2e")]
const fn device_session_id() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wired_management::{REQUEST_LINE_LEN, REQUEST_PREFIX, decode_request_line};
    use hidshift::HostId;
    use hidshift::management::{ManagementCommand, ManagementRequest};

    fn request_line(request: ManagementRequest) -> [u8; REQUEST_LINE_LEN] {
        let mut line = [0; REQUEST_LINE_LEN];
        line[..REQUEST_PREFIX.len()].copy_from_slice(REQUEST_PREFIX);
        for (index, byte) in request.encode().iter().copied().enumerate() {
            line[REQUEST_PREFIX.len() + index * 2] = hex_digit(byte >> 4);
            line[REQUEST_PREFIX.len() + index * 2 + 1] = hex_digit(byte & 0x0f);
        }
        line
    }

    const fn hex_digit(value: u8) -> u8 {
        if value < 10 {
            b'0' + value
        } else {
            b'A' + value - 10
        }
    }

    #[test]
    fn uart_line_decodes_the_same_management_request_as_ble() {
        assert!(REQUEST_LINE_LEN <= 64);
        let request = ManagementRequest {
            request_id: 0x2a,
            command: ManagementCommand::SelectHost(HostId(3)),
        };
        assert_eq!(decode_request_line(&request_line(request)), Some(request));
    }

    #[test]
    fn uart_line_ignores_logs_and_malformed_hex() {
        assert_eq!(decode_request_line(b"firmware: boot"), None);
        let mut malformed = request_line(ManagementRequest {
            request_id: 1,
            command: ManagementCommand::GetStatus,
        });
        malformed[REQUEST_PREFIX.len()] = b'z';
        assert_eq!(decode_request_line(&malformed), None);
        assert_eq!(decode_request_line(&malformed[..malformed.len() - 1]), None);
    }

    #[test]
    fn uart_line_accepts_uppercase_hex_and_all_commands() {
        for command in [
            ManagementCommand::GetStatus,
            ManagementCommand::SelectHost(HostId(1)),
            ManagementCommand::StartPairing(HostId(2)),
            ManagementCommand::ForgetHost(HostId(4)),
            ManagementCommand::GetHostInfo(HostId(3)),
        ] {
            let line = request_line(ManagementRequest {
                request_id: 0xaf,
                command,
            });
            assert_eq!(
                decode_request_line(&line).map(|request| request.request_id),
                Some(0xaf)
            );
        }
    }
}
