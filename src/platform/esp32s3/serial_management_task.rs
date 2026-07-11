use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Sender;
use embassy_time::Instant;
use esp_hal::peripherals::{GPIO44, UART0};
use esp_hal::uart::{Config, Uart};
use hidshift::management::{MANAGEMENT_REQUEST_LEN, ManagementDestination, ManagementRequest};
use hidshift::runtime::RUNTIME_INPUT_QUEUE_CAPACITY;
use hidshift::runtime::message::RuntimeInputMessage;

const REQUEST_PREFIX: &[u8] = b"@HIDSHIFT:";
const REQUEST_LINE_LEN: usize = REQUEST_PREFIX.len() + MANAGEMENT_REQUEST_LEN * 2;

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
    loop {
        match uart.read_async(&mut byte).await {
            Ok(1) if byte[0] == b'\n' || byte[0] == b'\r' => {
                if let Some(request) = decode_request_line(&line[..line_len]) {
                    sender
                        .send(RuntimeInputMessage::ManagementRequest {
                            destination: ManagementDestination::Wired,
                            request,
                            now_ms: Instant::now().as_millis(),
                        })
                        .await;
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

fn decode_request_line(line: &[u8]) -> Option<ManagementRequest> {
    if line.len() != REQUEST_LINE_LEN || !line.starts_with(REQUEST_PREFIX) {
        return None;
    }
    let encoded = &line[REQUEST_PREFIX.len()..];
    let mut request = [0u8; MANAGEMENT_REQUEST_LEN];
    for (index, output) in request.iter_mut().enumerate() {
        let high = hex_nibble(encoded[index * 2])?;
        let low = hex_nibble(encoded[index * 2 + 1])?;
        *output = (high << 4) | low;
    }
    ManagementRequest::decode(&request).ok()
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidshift::HostId;
    use hidshift::management::ManagementCommand;

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
