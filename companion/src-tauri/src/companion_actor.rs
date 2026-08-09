use std::collections::VecDeque;
use std::pin::Pin;

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures_util::StreamExt;
use hidshift::{
    HostId, MANAGEMENT_EVENT_UUID, MANAGEMENT_REQUEST_UUID, MANAGEMENT_RESPONSE_UUID,
    MANAGEMENT_SERVICE_UUID, ManagementClientSession, ManagementCommand, ManagementResponse,
    ManagementResponsePayload,
};
use hidshift_client::{
    ActiveTargetNotification, ClientError, ClientSessionTracker, ManagementClient,
};
use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::native_serial::NativeSerial;

pub enum ActorCommand {
    Connect(oneshot::Sender<Result<String, String>>),
    Request(
        ManagementCommand,
        oneshot::Sender<Result<ManagementResponse, String>>,
    ),
    Shutdown,
}

struct BluetoothConnection {
    peripheral: Peripheral,
    request: Characteristic,
    response: Characteristic,
}

enum Connection {
    Usb(NativeSerial),
    Bluetooth(BluetoothConnection),
}

impl Connection {
    fn label(&self) -> String {
        match self {
            Self::Usb(connection) => connection.label().into(),
            Self::Bluetooth(_) => "Bluetooth · HIDShift".into(),
        }
    }
}

type Reply = oneshot::Sender<Result<ManagementResponse, String>>;

pub struct CompanionActor {
    app: tauri::AppHandle,
    commands: mpsc::Receiver<ActorCommand>,
    client: ManagementClient,
    session: ClientSessionTracker,
    connection: Option<Connection>,
    notifications:
        Option<Pin<Box<dyn futures_util::Stream<Item = btleplug::api::ValueNotification> + Send>>>,
    current_reply: Option<Reply>,
    queued: VecDeque<(ManagementCommand, Reply)>,
    host_names: [Option<String>; 4],
    dual_s3: bool,
    reconnect_requested: bool,
    request_deadline: Option<tokio::time::Instant>,
    response_read_deadline: Option<tokio::time::Instant>,
}

const REQUEST_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(10);
const RESPONSE_READ_FALLBACK_DELAY: core::time::Duration = core::time::Duration::from_millis(250);

impl CompanionActor {
    pub fn new(app: tauri::AppHandle, commands: mpsc::Receiver<ActorCommand>) -> Self {
        Self {
            app,
            commands,
            client: ManagementClient::new(0),
            session: ClientSessionTracker::new(None),
            connection: None,
            notifications: None,
            current_reply: None,
            queued: VecDeque::new(),
            host_names: core::array::from_fn(|_| None),
            dual_s3: false,
            reconnect_requested: false,
            request_deadline: None,
            response_read_deadline: None,
        }
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(ActorCommand::Connect(reply)) => { let _ = reply.send(self.connect().await); }
                    Some(ActorCommand::Request(command, reply)) => self.enqueue(command, reply).await,
                    Some(ActorCommand::Shutdown) | None => break,
                },
                notification = async { self.notifications.as_mut().expect("guarded").next().await }, if self.notifications.is_some() => {
                    if let Some(notification) = notification { self.notification(notification).await; }
                    else { self.disconnect(); }
                },
                _ = tokio::time::sleep(core::time::Duration::from_secs(2)), if self.reconnect_requested => {
                    self.reconnect_requested = false;
                    if self.connect().await.is_err() { self.reconnect_requested = true; }
                },
                _ = async { tokio::time::sleep_until(self.request_deadline.expect("guarded")).await }, if self.request_deadline.is_some() => {
                    let _ = self.client.cancel();
                    self.request_deadline = None;
                    self.response_read_deadline = None;
                    if let Some(reply) = self.current_reply.take() { let _ = reply.send(Err("management request timed out".into())); }
                    self.disconnect();
                }
                _ = async { tokio::time::sleep_until(self.response_read_deadline.expect("guarded")).await }, if self.response_read_deadline.is_some() => {
                    self.read_response_fallback().await;
                }
                _ = tokio::time::sleep(core::time::Duration::from_secs(2)), if self.usb_connected() && self.current_reply.is_none() => {
                    if self.dual_s3 {
                        self.queue_internal(ManagementCommand::GetOutputTargetStatus).await;
                    } else {
                        self.queue_internal(ManagementCommand::GetStatus).await;
                    }
                }
            }
        }
    }

    async fn connect(&mut self) -> Result<String, String> {
        if let Some(connection) = self.connection.as_ref() {
            return Ok(connection.label());
        }
        let usb_result = tokio::task::spawn_blocking(NativeSerial::connect)
            .await
            .map_err(|error| format!("USB discovery task failed: {error}"))?;
        let usb_error = match usb_result {
            Ok(connection) => {
                let label = connection.label().to_string();
                self.connection = Some(Connection::Usb(connection));
                self.reconnect_requested = false;
                self.initialize_connection().await;
                return Ok(label);
            }
            Err(error) => error,
        };
        self.connect_bluetooth()
            .await
            .map_err(|error| format!("USB: {usb_error}; Bluetooth: {error}"))?;
        self.initialize_connection().await;
        Ok(self
            .connection
            .as_ref()
            .map(Connection::label)
            .unwrap_or_else(|| "HIDShift".into()))
    }

    async fn connect_bluetooth(&mut self) -> Result<(), String> {
        let manager = Manager::new().await.map_err(|error| error.to_string())?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or("no Bluetooth adapter")?;
        adapter
            .start_scan(ScanFilter {
                services: vec![parse_uuid(MANAGEMENT_SERVICE_UUID)?],
            })
            .await
            .map_err(|error| error.to_string())?;
        tokio::time::sleep(core::time::Duration::from_secs(2)).await;
        let service_uuid = parse_uuid(MANAGEMENT_SERVICE_UUID)?;
        let mut selected = None;
        for peripheral in adapter
            .peripherals()
            .await
            .map_err(|error| error.to_string())?
        {
            let properties = peripheral
                .properties()
                .await
                .map_err(|error| error.to_string())?;
            if properties.as_ref().is_some_and(|properties| {
                properties.local_name.as_deref() == Some("HIDShift")
                    || properties.services.contains(&service_uuid)
            }) {
                selected = Some(peripheral);
                break;
            }
        }
        adapter
            .stop_scan()
            .await
            .map_err(|error| error.to_string())?;
        let peripheral = selected.ok_or("HIDShift not found")?;
        if !peripheral
            .is_connected()
            .await
            .map_err(|error| error.to_string())?
        {
            peripheral
                .connect()
                .await
                .map_err(|error| error.to_string())?;
        }
        peripheral
            .discover_services()
            .await
            .map_err(|error| error.to_string())?;
        let request = characteristic(&peripheral, MANAGEMENT_REQUEST_UUID)?;
        let response = characteristic(&peripheral, MANAGEMENT_RESPONSE_UUID)?;
        let event = characteristic(&peripheral, MANAGEMENT_EVENT_UUID)?;
        let notifications = peripheral
            .notifications()
            .await
            .map_err(|error| error.to_string())?;
        peripheral
            .subscribe(&response)
            .await
            .map_err(|error| error.to_string())?;
        peripheral
            .subscribe(&event)
            .await
            .map_err(|error| error.to_string())?;
        self.notifications = Some(notifications);
        self.connection = Some(Connection::Bluetooth(BluetoothConnection {
            peripheral,
            request,
            response,
        }));
        self.reconnect_requested = false;
        Ok(())
    }

    async fn initialize_connection(&mut self) {
        self.queue_internal(ManagementCommand::GetClientSession)
            .await;
        self.queue_internal(ManagementCommand::GetSchema).await;
        self.queue_internal(ManagementCommand::GetStatus).await;
        for slot in 1..=4 {
            self.queue_internal(ManagementCommand::GetHostInfo(HostId(slot)))
                .await;
        }
    }

    fn usb_connected(&self) -> bool {
        matches!(self.connection, Some(Connection::Usb(_)))
    }

    async fn enqueue(&mut self, command: ManagementCommand, reply: Reply) {
        self.queued.push_back((command, reply));
        self.start_next().await;
    }

    async fn queue_internal(&mut self, command: ManagementCommand) {
        let (send, _receive) = oneshot::channel();
        self.enqueue(command, send).await;
    }

    async fn start_next(&mut self) {
        if self.current_reply.is_some() {
            return;
        }
        while let Some((command, reply)) = self.queued.pop_front() {
            if matches!(self.connection, Some(Connection::Usb(_))) {
                let result = match self.connection.as_mut() {
                    Some(Connection::Usb(connection)) => {
                        connection.request(&mut self.client, command)
                    }
                    _ => unreachable!(),
                };
                match result {
                    Ok(response) => {
                        self.observe_response(response);
                        let _ = reply.send(Ok(response));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        self.disconnect();
                        return;
                    }
                }
                continue;
            }

            let Some(Connection::Bluetooth(connection)) = self.connection.as_ref() else {
                let _ = reply.send(Err("disconnected".into()));
                return;
            };
            match self.client.begin(command) {
                Ok(pending) => match connection
                    .peripheral
                    .write(
                        &connection.request,
                        &pending.encode(),
                        WriteType::WithResponse,
                    )
                    .await
                {
                    Ok(()) => {
                        self.current_reply = Some(reply);
                        self.request_deadline = Some(tokio::time::Instant::now() + REQUEST_TIMEOUT);
                        self.response_read_deadline =
                            Some(tokio::time::Instant::now() + RESPONSE_READ_FALLBACK_DELAY);
                        return;
                    }
                    Err(error) => {
                        let _ = self.client.cancel();
                        let _ = reply.send(Err(error.to_string()));
                    }
                },
                Err(error) => {
                    let _ = reply.send(Err(format!("{error:?}")));
                }
            }
        }
    }

    async fn notification(&mut self, notification: btleplug::api::ValueNotification) {
        let event_uuid = match parse_uuid(MANAGEMENT_EVENT_UUID) {
            Ok(uuid) => uuid,
            Err(_) => return,
        };
        if notification.uuid == event_uuid {
            if self
                .session
                .accept_event(&notification.value)
                .unwrap_or(false)
            {
                if self.dual_s3 {
                    self.queue_internal(ManagementCommand::GetOutputTargetStatus)
                        .await;
                } else {
                    self.queue_internal(ManagementCommand::GetStatus).await;
                }
            }
            return;
        }
        let response_uuid = match parse_uuid(MANAGEMENT_RESPONSE_UUID) {
            Ok(uuid) => uuid,
            Err(_) => return,
        };
        let Ok(Some(response)) = accept_response_notification(
            &mut self.client,
            notification.uuid,
            response_uuid,
            &notification.value,
        ) else {
            return;
        };
        self.complete_response(response).await;
    }

    async fn read_response_fallback(&mut self) {
        self.response_read_deadline = None;
        let Some(Connection::Bluetooth(connection)) = self.connection.as_ref() else {
            return;
        };
        let response = connection.peripheral.read(&connection.response).await;
        if let Ok(bytes) = response
            && let Ok(Some(response)) = self.client.accept_notification(&bytes)
        {
            self.complete_response(response).await;
        }
        // A read is only a one-shot fallback for a notification lost around
        // subscription setup. Repeated ATT reads keep an otherwise idle host
        // connection active and can steal radio events from the HID target.
    }

    async fn complete_response(&mut self, response: ManagementResponse) {
        let load_output_status = matches!(
            response.payload,
            ManagementResponsePayload::Schema(schema)
                if schema.capabilities & hidshift::MANAGEMENT_CAPABILITY_DUAL_S3_WIRED != 0
        );
        self.observe_response(response);
        self.request_deadline = None;
        self.response_read_deadline = None;
        if let Some(reply) = self.current_reply.take() {
            let _ = reply.send(Ok(response));
        }
        self.start_next().await;
        if load_output_status {
            self.queue_internal(ManagementCommand::GetOutputTargetStatus)
                .await;
        }
    }

    fn observe_response(&mut self, response: ManagementResponse) {
        match response.payload {
            ManagementResponsePayload::ClientSession(ManagementClientSession { host_id }) => {
                self.session.set_local_host(host_id)
            }
            ManagementResponsePayload::Status(status) if !self.dual_s3 => {
                let notification = self.session.observe_status(status);
                self.notify_target_change(notification)
            }
            ManagementResponsePayload::OutputTargetStatus(status) => {
                let notification = self.session.observe_output_status(status);
                self.notify_target_change(notification)
            }
            ManagementResponsePayload::Schema(schema) => {
                self.dual_s3 =
                    schema.capabilities & hidshift::MANAGEMENT_CAPABILITY_DUAL_S3_WIRED != 0;
            }
            ManagementResponsePayload::HostInfo(info) => {
                if let Some(index) = info
                    .host_id
                    .0
                    .checked_sub(1)
                    .map(usize::from)
                    .filter(|index| *index < 4)
                {
                    self.host_names[index] = core::str::from_utf8(info.name.as_bytes())
                        .ok()
                        .map(str::to_owned);
                }
            }
            _ => {}
        }
    }

    fn notify_target_change(&mut self, notification: Option<ActiveTargetNotification>) {
        let Some(notification) = notification else {
            return;
        };
        let body = match notification {
            ActiveTargetNotification::ThisComputer => {
                "入力先がこのPCに切り替わりました".to_string()
            }
            ActiveTargetNotification::OtherComputer(host) => {
                format!("入力先を「{}」へ切り替えました", self.host_label(host))
            }
            ActiveTargetNotification::Wired => "入力先を「USB」へ切り替えました".to_string(),
        };
        let _ = self
            .app
            .notification()
            .builder()
            .title("HIDShift")
            .body(&body)
            .show();
        let _ = self.app.emit("hidshift://target-changed", body);
    }

    fn host_label(&self, host: HostId) -> String {
        host.0
            .checked_sub(1)
            .map(usize::from)
            .and_then(|index| self.host_names.get(index))
            .and_then(|name| name.clone())
            .unwrap_or_else(|| format!("スロット {}", host.0))
    }

    fn disconnect(&mut self) {
        let was_connected = self.connection.is_some();
        self.connection = None;
        self.notifications = None;
        self.session.disconnected();
        self.client.cancel();
        self.request_deadline = None;
        self.response_read_deadline = None;
        if let Some(reply) = self.current_reply.take() {
            let _ = reply.send(Err("disconnected".into()));
        }
        while let Some((_, reply)) = self.queued.pop_front() {
            let _ = reply.send(Err("disconnected".into()));
        }
        self.reconnect_requested |= was_connected;
    }
}

fn parse_uuid(value: &str) -> Result<Uuid, String> {
    Uuid::parse_str(value).map_err(|error| error.to_string())
}

fn characteristic(peripheral: &Peripheral, uuid: &str) -> Result<Characteristic, String> {
    let uuid = parse_uuid(uuid)?;
    peripheral
        .characteristics()
        .into_iter()
        .find(|item| item.uuid == uuid)
        .ok_or_else(|| format!("missing characteristic {uuid}"))
}

fn accept_response_notification(
    client: &mut ManagementClient,
    notification_uuid: Uuid,
    response_uuid: Uuid,
    bytes: &[u8],
) -> Result<Option<ManagementResponse>, ClientError> {
    if notification_uuid != response_uuid {
        return Ok(None);
    }
    client.accept_notification(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hidshift::{
        MANAGEMENT_RESPONSE_LEN, ManagementResponsePayload, ManagementResult, ManagementStatus,
    };

    fn response(request_id: u8) -> [u8; MANAGEMENT_RESPONSE_LEN] {
        ManagementResponse {
            request_id,
            result: ManagementResult::Ok,
            payload: ManagementResponsePayload::Status(ManagementStatus::empty(4)),
        }
        .encode()
    }

    #[test]
    fn only_expected_management_response_completes_the_pending_request() {
        let response_uuid = parse_uuid(MANAGEMENT_RESPONSE_UUID).unwrap();
        let hid_uuid = Uuid::parse_str("00002a4d-0000-1000-8000-00805f9b34fb").unwrap();
        let mut client = ManagementClient::new(7);
        client.begin(ManagementCommand::GetStatus).unwrap();

        assert_eq!(
            accept_response_notification(&mut client, hid_uuid, response_uuid, &response(7)),
            Ok(None)
        );
        assert_eq!(
            accept_response_notification(&mut client, response_uuid, response_uuid, &response(6)),
            Ok(None)
        );
        assert!(client.is_pending());
        assert_eq!(
            accept_response_notification(&mut client, response_uuid, response_uuid, &response(7)),
            Ok(Some(ManagementResponse::decode(&response(7)).unwrap()))
        );
        assert!(!client.is_pending());
    }
}
