use std::io::{ErrorKind, Write};
use std::time::{Duration, Instant};

use hidshift::{ManagementCommand, ManagementResponse};
use hidshift_client::{ManagementClient, SerialResponseDecoder, encode_serial_request};
use serialport::{SerialPort, SerialPortInfo, SerialPortType};

const BAUD_RATE: u32 = 115_200;
const IO_TIMEOUT: Duration = Duration::from_millis(100);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

pub struct NativeSerial {
    port: Box<dyn SerialPort>,
    decoder: SerialResponseDecoder,
    label: String,
}

impl NativeSerial {
    pub fn connect() -> Result<Self, String> {
        let ports = serialport::available_ports().map_err(|error| error.to_string())?;
        let mut failures = Vec::new();
        for port_name in usb_serial_port_names(ports) {
            match Self::probe(&port_name) {
                Ok(connection) => return Ok(connection),
                Err(error) => failures.push(format!("{port_name}: {error}")),
            }
        }
        if failures.is_empty() {
            Err("no USB serial device is connected".into())
        } else {
            Err(format!(
                "no USB serial device answered the HIDShift protocol ({})",
                failures.join("; ")
            ))
        }
    }

    fn probe(port_name: &str) -> Result<Self, String> {
        let mut port = serialport::new(port_name, BAUD_RATE)
            .timeout(IO_TIMEOUT)
            .open()
            .map_err(|error| error.to_string())?;
        let mut decoder = SerialResponseDecoder::default();
        let mut client = ManagementClient::new(0);
        let deadline = Instant::now() + PROBE_TIMEOUT;
        while Instant::now() < deadline {
            let pending = client
                .begin(ManagementCommand::GetStatus)
                .map_err(|error| format!("{error:?}"))?;
            port.write_all(&encode_serial_request(pending))
                .map_err(|error| error.to_string())?;
            let attempt_deadline = (Instant::now() + Duration::from_millis(500)).min(deadline);
            if read_response(&mut *port, &mut decoder, &mut client, attempt_deadline)?.is_some() {
                return Ok(Self {
                    port,
                    decoder,
                    label: format!("USB · {port_name}"),
                });
            }
            let _ = client.cancel();
        }
        Err("HIDShift management probe timed out".into())
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn request(
        &mut self,
        client: &mut ManagementClient,
        command: ManagementCommand,
    ) -> Result<ManagementResponse, String> {
        let pending = client
            .begin(command)
            .map_err(|error| format!("{error:?}"))?;
        self.port
            .write_all(&encode_serial_request(pending))
            .map_err(|error| error.to_string())?;
        match read_response(
            &mut *self.port,
            &mut self.decoder,
            client,
            Instant::now() + REQUEST_TIMEOUT,
        )? {
            Some(response) => Ok(response),
            None => {
                let _ = client.cancel();
                Err("USB management request timed out".into())
            }
        }
    }
}

fn usb_serial_port_names(ports: Vec<SerialPortInfo>) -> impl Iterator<Item = String> {
    ports.into_iter().filter_map(|port| {
        matches!(port.port_type, SerialPortType::UsbPort(_)).then_some(port.port_name)
    })
}

fn read_response(
    port: &mut dyn SerialPort,
    decoder: &mut SerialResponseDecoder,
    client: &mut ManagementClient,
    deadline: Instant,
) -> Result<Option<ManagementResponse>, String> {
    let mut bytes = [0; 256];
    while Instant::now() < deadline {
        match port.read(&mut bytes) {
            Ok(length) => {
                for response in decoder.push(&bytes[..length]) {
                    match client.accept_notification(&response) {
                        Ok(Some(response)) => return Ok(Some(response)),
                        Ok(None) => {}
                        Err(error) => return Err(format!("{error:?}")),
                    }
                }
            }
            Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serialport::UsbPortInfo;

    #[test]
    fn discovery_uses_os_usb_candidates_without_device_specific_paths() {
        let ports = vec![
            SerialPortInfo {
                port_name: "/dev/ttyS0".into(),
                port_type: SerialPortType::Unknown,
            },
            SerialPortInfo {
                port_name: "/dev/serial/by-id/runtime-value".into(),
                port_type: SerialPortType::UsbPort(UsbPortInfo {
                    vid: 1,
                    pid: 2,
                    serial_number: None,
                    manufacturer: None,
                    product: None,
                }),
            },
        ];

        assert_eq!(
            usb_serial_port_names(ports).collect::<Vec<_>>(),
            ["/dev/serial/by-id/runtime-value"]
        );
    }
}
