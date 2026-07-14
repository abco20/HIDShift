use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::Instant;
use esp_hal::peripherals::{GPIO44, UART0};
use esp_hal::uart::{Config, Uart};
use hidshift::InputTransport;
use hidshift::management::ManagementDestination;
use hidshift::runtime::RUNTIME_INPUT_QUEUE_CAPACITY;
use hidshift::runtime::message::RuntimeInputMessage;

#[cfg(feature = "hardware-e2e")]
use hidshift::e2e::{E2eCommand, E2ePacket};

#[embassy_executor::task]
pub async fn serial_management_task(
    sender: Sender<
        'static,
        CriticalSectionRawMutex,
        RuntimeInputMessage,
        RUNTIME_INPUT_QUEUE_CAPACITY,
    >,
    uart: UART0<'static>,
    rx: GPIO44<'static>,
    boot_session_id: u32,
) {
    let Ok(uart) = Uart::new(uart, Config::default()) else {
        log::error!("firmware: management UART init failed");
        return;
    };
    let mut uart = uart.with_rx(rx).into_async();
    let mut line = [0u8; 64];
    let mut line_len = 0usize;
    let mut byte = [0u8; 1];

    log::info!("firmware: wired management ready on UART0 RX GPIO44");
    #[cfg(feature = "hardware-e2e")]
    log::info!("@HIDSHIFT-E2E:READY,1");
    loop {
        match uart.read_async(&mut byte).await {
            Ok(1) if byte[0] == b'\n' || byte[0] == b'\r' => {
                if let Some(request) =
                    crate::wired_management::decode_request_line(&line[..line_len])
                {
                    if cfg!(feature = "espnow") && request.command.is_espnow_pairing() {
                        super::storage_task::espnow_management_sender()
                            .send(super::storage_task::EspNowManagementRequest {
                                destination: ManagementDestination::Wired,
                                request,
                            })
                            .await;
                    } else {
                        sender
                            .send(RuntimeInputMessage::ManagementRequest {
                                destination: ManagementDestination::Wired,
                                request,
                                now_ms: Instant::now().as_millis(),
                            })
                            .await;
                    }
                }
                #[cfg(feature = "hardware-e2e")]
                if let Ok(packet) = E2ePacket::decode_line(&line[..line_len]) {
                    let sequence = packet.sequence;
                    let acknowledge = packet.requests_acknowledgement();
                    let ingress_us = Instant::now().as_micros();
                    if matches!(packet.command, E2eCommand::Hello) {
                        crate::e2e_telemetry::reset_espnow_timings();
                        log::info!(
                            "@HIDSHIFT-BRIDGE:CLOCK,{},{},{},{}",
                            sequence,
                            boot_session_id,
                            device_session_id(),
                            Instant::now().as_micros()
                        );
                    }
                    if let E2eCommand::SelectTransport { transport } = packet.command {
                        super::transport_route::select(transport);
                        log::info!("@HIDSHIFT-BRIDGE:ROUTE,{:?}", transport);
                    }
                    if packet.carries_input() {
                        crate::e2e_telemetry::record_ingress(sequence, ingress_us);
                        #[cfg(feature = "espnow")]
                        if super::transport_route::routes_to(InputTransport::EspNow) {
                            super::espnow_link_task::forward_e2e_packet(packet, ingress_us).await;
                            // Channel send is normally immediately ready. Yield
                            // explicitly so the awakened radio owner can submit
                            // the realtime frame before this UART task performs
                            // any lower-priority decoding or acknowledgements.
                            embassy_futures::yield_now().await;
                        }
                    }
                    if matches!(packet.command, E2eCommand::EnterDeviceDownload) {
                        #[cfg(feature = "espnow")]
                        super::espnow_link_task::forward_e2e_packet(packet, ingress_us).await;
                    }
                    if let E2eCommand::ReadTimestamp { target_sequence } = packet.command {
                        let snapshot = crate::e2e_telemetry::snapshot(target_sequence);
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
                        log::info!(
                            "@HIDSHIFT-BRIDGE:STAMP,{},{},{},{},{},{},{},{},{},{},{},{}",
                            sequence,
                            boot_session_id,
                            snapshot.espnow_sequence,
                            snapshot.espnow_ingress_us,
                            snapshot.espnow_enqueue_us,
                            snapshot.espnow_dequeue_us,
                            snapshot.espnow_send_start_us,
                            snapshot.espnow_tx_done_us,
                            snapshot.device_sequence,
                            snapshot.device_radio_rx_us,
                            snapshot.device_reassembled_us,
                            snapshot.device_hid_write_us
                        );
                    }
                    match packet.input_frames() {
                        Ok(frames) => {
                            for frame in frames.into_iter().flatten() {
                                // The standalone ESP-NOW image has no BLE
                                // consumer. Feeding the same synthetic input
                                // through the dormant runtime as well as the
                                // radio path delays the realtime sender for no
                                // observable output. A coexistence image uses
                                // an explicit transport-routing policy instead
                                // of restoring this unconditional duplicate.
                                if super::transport_route::routes_to(InputTransport::Ble) {
                                    sender
                                        .send(RuntimeInputMessage::BridgeEvent(
                                            hidshift::BridgeEvent::InputFrame(frame),
                                        ))
                                        .await;
                                }
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

#[cfg(all(feature = "hardware-e2e", feature = "espnow"))]
fn device_session_id() -> u32 {
    super::espnow_link_task::device_session_id()
}

#[cfg(all(feature = "hardware-e2e", not(feature = "espnow")))]
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
