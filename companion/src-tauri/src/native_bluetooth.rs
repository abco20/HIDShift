use std::pin::Pin;

use btleplug::api::{
    Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Manager, Peripheral};
use futures_util::{Stream, StreamExt};
use hidshift::{
    HostId, MANAGEMENT_EVENT_UUID, MANAGEMENT_REQUEST_UUID, MANAGEMENT_RESPONSE_UUID,
    MANAGEMENT_SERVICE_UUID, ManagementCommand, ManagementResponsePayload,
};
use hidshift_client::ManagementClient;
use uuid::Uuid;

pub type NotificationStream = Pin<Box<dyn Stream<Item = btleplug::api::ValueNotification> + Send>>;

pub struct NativeBluetooth {
    pub peripheral: Peripheral,
    pub request: Characteristic,
    pub response: Characteristic,
    pub notifications: Option<NotificationStream>,
}

impl NativeBluetooth {
    pub async fn connect() -> Result<Self, String> {
        let manager = Manager::new().await.map_err(|error| error.to_string())?;
        let adapter = manager
            .adapters()
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or("no Bluetooth adapter")?;
        let service_uuid = parse_uuid(MANAGEMENT_SERVICE_UUID)?;
        adapter
            .start_scan(ScanFilter {
                services: vec![service_uuid],
            })
            .await
            .map_err(|error| error.to_string())?;
        tokio::time::sleep(core::time::Duration::from_secs(2)).await;
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
        Ok(Self {
            peripheral,
            request,
            response,
            notifications: Some(Box::pin(notifications)),
        })
    }

    pub async fn probe_local_host() -> Result<Option<HostId>, String> {
        let mut connection = Self::connect().await?;
        let mut notifications = connection
            .notifications
            .take()
            .ok_or("Bluetooth notification stream missing")?;
        let mut client = ManagementClient::new(0xa0);
        let pending = client
            .begin(ManagementCommand::GetClientSession)
            .map_err(|error| format!("{error:?}"))?;
        connection
            .peripheral
            .write(
                &connection.request,
                &pending.encode(),
                WriteType::WithResponse,
            )
            .await
            .map_err(|error| error.to_string())?;
        let deadline = tokio::time::Instant::now() + core::time::Duration::from_secs(3);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err("Bluetooth identity request timed out".into());
            }
            match tokio::time::timeout(remaining, notifications.next()).await {
                Ok(Some(notification)) if notification.uuid == connection.response.uuid => {
                    if let Ok(Some(response)) = client.accept_notification(&notification.value) {
                        return Ok(client_session_host(response.payload));
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err("Bluetooth notification stream ended".into()),
                Err(_) => {
                    let bytes = connection
                        .peripheral
                        .read(&connection.response)
                        .await
                        .map_err(|error| error.to_string())?;
                    return match client.accept_notification(&bytes) {
                        Ok(Some(response)) => Ok(client_session_host(response.payload)),
                        _ => Err("Bluetooth identity response missing".into()),
                    };
                }
            }
        }
    }
}

fn client_session_host(payload: ManagementResponsePayload) -> Option<HostId> {
    match payload {
        ManagementResponsePayload::ClientSession(session) => session.host_id,
        _ => None,
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
