use embassy_time::Timer;

use super::{flash_backend, wired_management};

#[embassy_executor::task]
pub async fn task(
    storage: &'static mut flash_backend::FirmwareStorageBackend,
    uart: esp_hal::peripherals::UART0<'static>,
    rx: esp_hal::peripherals::GPIO44<'static>,
) {
    let Ok(uart) = esp_hal::uart::Uart::new(uart, esp_hal::uart::Config::default()) else {
        log::error!("bridge-device: management UART init failed");
        return;
    };
    let mut uart = uart.with_rx(rx).into_async();
    let mut service = hidshift::espnow_pairing_management::EspNowPairingService::new(
        hidshift::espnow_pairing::EspNowRole::UsbDevice,
        storage.restored_pairing(),
    );
    let mut line = [0u8; 64];
    let mut line_len = 0usize;
    let mut byte = [0u8; 1];
    log::info!("bridge-device: wired management ready");
    loop {
        match uart.read_async(&mut byte).await {
            Ok(1) if byte[0] == b'\n' || byte[0] == b'\r' => {
                if let Some(request) = wired_management::decode_request_line(&line[..line_len]) {
                    let local = esp_hal::efuse::interface_mac_address(
                        esp_hal::efuse::InterfaceMacAddress::Station,
                    );
                    let mut outcome = service.handle(
                        request.command,
                        local.as_bytes().try_into().unwrap_or([0; 6]),
                    );
                    let restart = match outcome.action {
                        hidshift::espnow_pairing_management::EspNowPairingAction::None => false,
                        hidshift::espnow_pairing_management::EspNowPairingAction::Persist(
                            pairing,
                        ) => {
                            if storage.write_pairing(pairing).is_ok() {
                                service.persisted(pairing);
                                true
                            } else {
                                outcome.result = hidshift::ManagementResult::InternalError;
                                false
                            }
                        }
                        hidshift::espnow_pairing_management::EspNowPairingAction::Clear => {
                            if storage.clear_pairing().is_ok() {
                                service.cleared();
                                true
                            } else {
                                outcome.result = hidshift::ManagementResult::InternalError;
                                false
                            }
                        }
                    };
                    wired_management::print_response(hidshift::ManagementResponse {
                        request_id: request.request_id,
                        result: outcome.result,
                        payload: outcome.payload,
                    });
                    if restart && outcome.result == hidshift::ManagementResult::Ok {
                        Timer::after_millis(100).await;
                        esp_hal::system::software_reset();
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
            Err(error) => log::warn!("bridge-device: management UART read failed: {:?}", error),
        }
    }
}
