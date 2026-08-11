use std::error::Error;
use std::time::{Duration, Instant};

use hidshift::{
    ManagementCommand, ManagementResponse, ManagementResponsePayload, ManagementResult,
};
use hidshift_client::{
    ManagementClient,
    debug_serial::{SerialResponseDecoder, encode_serial_request},
};
use serialport::SerialPort;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const READY_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> Result<(), Box<dyn Error>> {
    let port_name = std::env::args()
        .nth(1)
        .ok_or("usage: serial_management_smoke <debug-serial-port>")?;
    let mut port = serialport::new(&port_name, 115_200)
        .timeout(Duration::from_millis(100))
        .open()?;
    wait_until_ready(&mut *port)?;

    let status = request(&mut *port, ManagementCommand::GetStatus, 0x40)?;
    require_ok(status)?;
    let ManagementResponsePayload::Status(status_payload) = status.payload else {
        return Err("status payload missing".into());
    };
    for index in 0..status_payload.usb.device_count {
        require_ok(request(
            &mut *port,
            ManagementCommand::GetUsbDevice {
                index,
                name_offset: 0,
            },
            0x50u8.wrapping_add(index),
        )?)?;
    }
    require_ok(request(
        &mut *port,
        ManagementCommand::GetDiagnostics,
        0x60,
    )?)?;
    println!(
        "{}",
        serde_json::json!({
            "schema_version": 1,
            "debug_transport": "uart",
            "usb_devices": status_payload.usb.device_count,
            "ok": true,
        })
    );
    Ok(())
}

fn wait_until_ready(port: &mut dyn SerialPort) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut request_id = 0u8;
    let mut last_error = None;
    while Instant::now() < deadline {
        match request(port, ManagementCommand::GetStatus, request_id) {
            Ok(response) if response.result == ManagementResult::Ok => return Ok(()),
            Ok(response) => last_error = Some(format!("firmware returned {:?}", response.result)),
            Err(error) => last_error = Some(error.to_string()),
        }
        request_id = request_id.wrapping_add(1);
    }
    Err(format!(
        "debug UART management did not become ready: {}",
        last_error.unwrap_or_else(|| "no response".into())
    )
    .into())
}

fn request(
    port: &mut dyn SerialPort,
    command: ManagementCommand,
    request_id: u8,
) -> Result<ManagementResponse, Box<dyn Error>> {
    let mut client = ManagementClient::new(request_id);
    let pending = client
        .begin(command)
        .map_err(|error| format!("could not encode request: {error:?}"))?;
    port.write_all(&encode_serial_request(pending))?;
    port.flush()?;
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    let mut decoder = SerialResponseDecoder::default();
    let mut bytes = [0; 256];
    while Instant::now() < deadline {
        match port.read(&mut bytes) {
            Ok(0) => {}
            Ok(length) => {
                for response in decoder.push(&bytes[..length]) {
                    if let Some(response) = client
                        .accept_notification(&response)
                        .map_err(|error| format!("invalid response: {error:?}"))?
                    {
                        return Ok(response);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("debug UART management request timed out".into())
}

fn require_ok(response: ManagementResponse) -> Result<(), Box<dyn Error>> {
    if response.result == ManagementResult::Ok {
        Ok(())
    } else {
        Err(format!("firmware returned {:?}", response.result).into())
    }
}
