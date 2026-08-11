use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hidapi::{BusType, DeviceInfo, HidApi, HidDevice};
use hidshift::{
    MANAGEMENT_EVENT_LEN, MANAGEMENT_RESPONSE_LEN, ManagementCommand, ManagementResponse,
};
use hidshift_client::{
    HidManagementFrame, ManagementClient, PendingRequest, decode_hid_input_packet,
    encode_hid_request, is_management_hid_identity,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

const IO_TIMEOUT_MS: i32 = 50;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

pub struct NativeHid {
    requests: Sender<PendingRequest>,
    responses: Receiver<[u8; MANAGEMENT_RESPONSE_LEN]>,
    events: Option<UnboundedReceiver<NativeHidEvent>>,
    worker_stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeHidEvent {
    Management([u8; MANAGEMENT_EVENT_LEN]),
    Disconnected(String),
}

struct SpawnedWorker {
    requests: Sender<PendingRequest>,
    responses: Receiver<[u8; MANAGEMENT_RESPONSE_LEN]>,
    events: UnboundedReceiver<NativeHidEvent>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl NativeHid {
    pub fn connect() -> Result<Self, String> {
        let api = HidApi::new().map_err(|error| error.to_string())?;
        let candidates = api
            .device_list()
            .filter(|device| is_management_interface(device))
            .collect::<Vec<_>>();
        let device_info = match candidates.as_slice() {
            [] => return Err("no HIDShift management HID interface is connected".into()),
            [device] => *device,
            _ => return Err("multiple HIDShift management HID interfaces are connected".into()),
        };
        let label = device_info
            .product_string()
            .map(|product| format!("USB HID · {product}"))
            .unwrap_or_else(|| "USB HID · HIDShift".into());
        let device = device_info
            .open_device(&api)
            .map_err(|error| error.to_string())?;
        probe(&device)?;
        let worker = spawn_worker(device)?;
        Ok(Self {
            requests: worker.requests,
            responses: worker.responses,
            events: Some(worker.events),
            worker_stop: worker.stop,
            worker: Some(worker.thread),
            label,
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn take_events(&mut self) -> Option<UnboundedReceiver<NativeHidEvent>> {
        self.events.take()
    }

    pub fn request(
        &mut self,
        client: &mut ManagementClient,
        command: ManagementCommand,
    ) -> Result<ManagementResponse, String> {
        let pending = client
            .begin(command)
            .map_err(|error| format!("{error:?}"))?;
        self.requests.send(pending).map_err(|_| {
            let _ = client.cancel();
            "USB HID management connection closed".to_string()
        })?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                let _ = client.cancel();
                return Err("USB HID management request timed out".into());
            };
            match self.responses.recv_timeout(remaining) {
                Ok(bytes) => match client.accept_notification(&bytes) {
                    Ok(Some(response)) => return Ok(response),
                    Ok(None) => {}
                    Err(error) => {
                        let _ = client.cancel();
                        return Err(format!("{error:?}"));
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let _ = client.cancel();
                    return Err("USB HID management request timed out".into());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = client.cancel();
                    return Err("USB HID management connection closed".into());
                }
            }
        }
    }
}

impl Drop for NativeHid {
    fn drop(&mut self) {
        self.worker_stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn is_management_interface(device: &DeviceInfo) -> bool {
    matches!(device.bus_type(), BusType::Usb)
        && is_management_hid_identity(
            device.vendor_id(),
            device.product_id(),
            device.usage_page(),
            device.usage(),
        )
}

fn probe(device: &HidDevice) -> Result<(), String> {
    let mut client = ManagementClient::new(0);
    let pending = client
        .begin(ManagementCommand::GetStatus)
        .map_err(|error| format!("{error:?}"))?;
    device
        .write(&encode_hid_request(pending))
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let mut bytes = [0; 64];
    while Instant::now() < deadline {
        let length = device
            .read_timeout(&mut bytes, IO_TIMEOUT_MS)
            .map_err(|error| error.to_string())?;
        if let Some(HidManagementFrame::Response(response)) =
            decode_hid_input_packet(&bytes[..length])
            && client
                .accept_notification(&response)
                .map_err(|error| format!("{error:?}"))?
                .is_some()
        {
            return Ok(());
        }
    }
    Err("HIDShift management HID probe timed out".into())
}

fn spawn_worker(device: HidDevice) -> Result<SpawnedWorker, String> {
    let (request_sender, requests) = mpsc::channel::<PendingRequest>();
    let (response_sender, responses) = mpsc::channel();
    let (event_sender, events) = tokio::sync::mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("hidshift-hid-worker".into())
        .spawn(move || {
            hid_worker(
                &device,
                &requests,
                &response_sender,
                &event_sender,
                &thread_stop,
            );
        })
        .map_err(|error| format!("failed to start USB HID worker: {error}"))?;
    Ok(SpawnedWorker {
        requests: request_sender,
        responses,
        events,
        stop,
        thread,
    })
}

fn hid_worker(
    device: &HidDevice,
    requests: &Receiver<PendingRequest>,
    responses: &Sender<[u8; MANAGEMENT_RESPONSE_LEN]>,
    events: &UnboundedSender<NativeHidEvent>,
    stop: &AtomicBool,
) {
    let mut bytes = [0; 64];
    while !stop.load(Ordering::Acquire) {
        while let Ok(request) = requests.try_recv() {
            if let Err(error) = device.write(&encode_hid_request(request)) {
                let _ = events.send(NativeHidEvent::Disconnected(error.to_string()));
                return;
            }
        }
        match device.read_timeout(&mut bytes, IO_TIMEOUT_MS) {
            Ok(0) => {}
            Ok(length) => match decode_hid_input_packet(&bytes[..length]) {
                Some(HidManagementFrame::Response(response))
                    if responses.send(response).is_err() =>
                {
                    return;
                }
                Some(HidManagementFrame::Response(_)) => {}
                Some(HidManagementFrame::Event(event)) => {
                    let _ = events.send(NativeHidEvent::Management(event));
                }
                None => {}
            },
            Err(error) => {
                let _ = events.send(NativeHidEvent::Disconnected(error.to_string()));
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn management_interface_identity_is_distinct_from_keyboard_and_mouse_collections() {
        assert_ne!(hidshift::MANAGEMENT_HID_USAGE_PAGE, 0x01);
        assert_ne!(hidshift::MANAGEMENT_HID_USAGE, 0x06);
    }
}
