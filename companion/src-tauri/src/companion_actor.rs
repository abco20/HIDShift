use std::collections::VecDeque;

use btleplug::api::{Peripheral as _, WriteType};
use futures_util::StreamExt;
use hidshift::{
    HostId, MANAGEMENT_EVENT_UUID, MANAGEMENT_RESPONSE_UUID, ManagementClientSession,
    ManagementCommand, ManagementOutputTarget, ManagementOutputTargetStatus, ManagementResponse,
    ManagementResponsePayload, ManagementStatus, ManagementUsbPresentationKind,
};
use hidshift_client::{
    ActiveTargetNotification, ClientError, ClientSessionTracker, ManagementClient,
};
use hidshift_manager_ui::{
    DestinationRoute, LocalComputerIdentity, select_destination_command,
    target_belongs_to_local_computer,
};
use tauri::Emitter;
use tauri_plugin_autostart::ManagerExt;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::native_bluetooth::{NativeBluetooth, NotificationStream};
use crate::native_hid::{NativeHid, NativeHidEvent};
use crate::native_notification::DesktopNotifier;
use crate::native_state::{
    CompanionSnapshot, LocalRoutePolicy, NativeConnectionKind, NativePreferences, RouteDecision,
    WiredHostLinkRegistration, firmware_host_name, load_preferences, os_computer_name,
    retain_registered_local_host, save_preferences, standard_wired_present,
};

pub enum ActorCommand {
    Connect(oneshot::Sender<Result<String, String>>),
    Snapshot(oneshot::Sender<CompanionSnapshot>),
    SetNotifications {
        enabled: bool,
        reply: oneshot::Sender<Result<CompanionSnapshot, String>>,
    },
    SetAutostartState(bool),
    SelectThisComputer,
    SelectDestination(DestinationRoute),
    Request(
        ManagementCommand,
        oneshot::Sender<Result<ManagementResponse, String>>,
    ),
    Shutdown,
}

enum Connection {
    Usb(NativeHid),
    Bluetooth(NativeBluetooth),
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
    notifier: DesktopNotifier,
    commands: mpsc::Receiver<ActorCommand>,
    client: ManagementClient,
    session: ClientSessionTracker,
    connection: Option<Connection>,
    notifications: Option<NotificationStream>,
    hid_events: Option<mpsc::UnboundedReceiver<NativeHidEvent>>,
    current_reply: Option<Reply>,
    queued: VecDeque<(ManagementCommand, Reply)>,
    host_names: [Option<String>; 4],
    dual_s3: bool,
    computer_target_links: bool,
    reconnect_requested: bool,
    request_deadline: Option<tokio::time::Instant>,
    response_read_deadline: Option<tokio::time::Instant>,
    status: Option<ManagementStatus>,
    output_status: Option<ManagementOutputTargetStatus>,
    host_name_sources: [u8; 4],
    host_info_loaded: [bool; 4],
    computer_name: String,
    local_ble_host: Option<HostId>,
    local_wired_present: bool,
    host_name_write_attempted: Option<HostId>,
    wired_host_link: WiredHostLinkRegistration,
    route_policy: LocalRoutePolicy,
    route_switch_pending: bool,
    auto_notification: Option<AutoRouteNotification>,
    preferences: NativePreferences,
    autostart_enabled: bool,
    snapshot_revision: u64,
    device_revision: u64,
    usb_probe: Option<tokio::task::JoinHandle<Result<NativeHid, String>>>,
    wired_probe: Option<tokio::task::JoinHandle<bool>>,
    ble_identity_probe: Option<tokio::task::JoinHandle<Result<Option<HostId>, String>>>,
    identity_probe_due: tokio::time::Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AutoRouteNotification {
    FallbackToBluetooth,
    Silent,
}

const REQUEST_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(10);
const RESPONSE_READ_FALLBACK_DELAY: core::time::Duration = core::time::Duration::from_millis(250);

impl CompanionActor {
    pub fn new(app: tauri::AppHandle, commands: mpsc::Receiver<ActorCommand>) -> Self {
        let preferences = load_preferences(&app);
        let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
        let notifier = DesktopNotifier::new(app.clone());
        Self {
            app,
            notifier,
            commands,
            client: ManagementClient::new(0),
            session: ClientSessionTracker::new(None),
            connection: None,
            notifications: None,
            hid_events: None,
            current_reply: None,
            queued: VecDeque::new(),
            host_names: core::array::from_fn(|_| None),
            dual_s3: false,
            computer_target_links: false,
            reconnect_requested: false,
            request_deadline: None,
            response_read_deadline: None,
            status: None,
            output_status: None,
            host_name_sources: [0; 4],
            host_info_loaded: [false; 4],
            computer_name: os_computer_name(),
            local_ble_host: None,
            local_wired_present: false,
            host_name_write_attempted: None,
            wired_host_link: WiredHostLinkRegistration::default(),
            route_policy: LocalRoutePolicy::default(),
            route_switch_pending: false,
            auto_notification: None,
            preferences,
            autostart_enabled,
            snapshot_revision: 0,
            device_revision: 0,
            usb_probe: None,
            wired_probe: None,
            ble_identity_probe: None,
            identity_probe_due: tokio::time::Instant::now(),
        }
    }

    pub async fn run(mut self) {
        let mut maintenance = tokio::time::interval(core::time::Duration::from_secs(2));
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut reconnect = tokio::time::interval_at(
            tokio::time::Instant::now() + core::time::Duration::from_secs(2),
            core::time::Duration::from_secs(2),
        );
        reconnect.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut reconciliation = tokio::time::interval_at(
            tokio::time::Instant::now() + core::time::Duration::from_secs(30),
            core::time::Duration::from_secs(30),
        );
        reconciliation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                command = self.commands.recv() => match command {
                    Some(ActorCommand::Connect(reply)) => { let _ = reply.send(self.connect().await); }
                    Some(ActorCommand::Snapshot(reply)) => { let _ = reply.send(self.snapshot()); }
                    Some(ActorCommand::SetNotifications { enabled, reply }) => {
                        self.preferences.notifications_enabled = enabled;
                        self.preferences.notification_prompt_seen = true;
                        let result = save_preferences(&self.app, &self.preferences)
                            .map(|()| {
                                self.publish_snapshot();
                                self.snapshot()
                            });
                        let _ = reply.send(result);
                    }
                    Some(ActorCommand::SetAutostartState(enabled)) => {
                        self.autostart_enabled = enabled;
                        self.publish_snapshot();
                    }
                    Some(ActorCommand::SelectThisComputer) => self.select_this_computer().await,
                    Some(ActorCommand::SelectDestination(route)) => self.select_destination(route).await,
                    Some(ActorCommand::Request(command, reply)) => self.enqueue(command, reply).await,
                    Some(ActorCommand::Shutdown) | None => break,
                },
                notification = async { self.notifications.as_mut().expect("guarded").next().await }, if self.notifications.is_some() => {
                    if let Some(notification) = notification { self.notification(notification).await; }
                    else { self.disconnect(); }
                },
                hid_event = async { self.hid_events.as_mut().expect("guarded").recv().await }, if self.hid_events.is_some() => {
                    if let Some(event) = hid_event { self.hid_event(event).await; }
                    else { self.disconnect(); }
                },
                _ = reconnect.tick(), if self.reconnect_requested => {
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
                _ = reconciliation.tick(), if self.usb_connected() && self.current_reply.is_none() => {
                    if self.dual_s3 {
                        self.queue_internal(ManagementCommand::GetOutputTargetStatus).await;
                    } else {
                        self.queue_internal(ManagementCommand::GetStatus).await;
                    }
                }
                _ = maintenance.tick() => self.start_maintenance_probes(),
                result = async { self.usb_probe.as_mut().expect("guarded").await }, if self.usb_probe.is_some() => {
                    self.usb_probe = None;
                    if let Ok(Ok(connection)) = result {
                        self.promote_usb(connection).await;
                    }
                }
                result = async { self.wired_probe.as_mut().expect("guarded").await }, if self.wired_probe.is_some() => {
                    self.wired_probe = None;
                    if let Ok(present) = result && self.local_wired_present != present {
                        self.local_wired_present = present;
                        self.maybe_register_wired_host_link();
                        self.reconcile_local_route();
                        self.publish_snapshot();
                        self.start_next().await;
                    }
                }
                result = async { self.ble_identity_probe.as_mut().expect("guarded").await }, if self.ble_identity_probe.is_some() => {
                    self.ble_identity_probe = None;
                    match result {
                        Ok(Ok(Some(host))) => {
                            self.set_local_ble_host(host);
                            self.start_next().await;
                        }
                        _ => self.identity_probe_due = tokio::time::Instant::now() + core::time::Duration::from_secs(30),
                    }
                }
            }
        }
    }

    async fn connect(&mut self) -> Result<String, String> {
        if let Some(connection) = self.connection.as_ref() {
            return Ok(connection.label());
        }
        let usb_result = tokio::task::spawn_blocking(NativeHid::connect)
            .await
            .map_err(|error| format!("USB discovery task failed: {error}"))?;
        let usb_error = match usb_result {
            Ok(mut connection) => {
                let label = connection.label().to_string();
                self.hid_events = connection.take_events();
                self.connection = Some(Connection::Usb(connection));
                self.reconnect_requested = false;
                self.initialize_connection().await;
                self.publish_snapshot();
                return Ok(label);
            }
            Err(error) => error,
        };
        if let Err(error) = self.connect_bluetooth().await {
            self.reconnect_requested = true;
            self.publish_snapshot();
            return Err(format!("USB: {usb_error}; Bluetooth: {error}"));
        }
        self.initialize_connection().await;
        self.publish_snapshot();
        Ok(self
            .connection
            .as_ref()
            .map(Connection::label)
            .unwrap_or_else(|| "HIDShift".into()))
    }

    async fn connect_bluetooth(&mut self) -> Result<(), String> {
        let mut connection = NativeBluetooth::connect().await?;
        self.notifications = connection.notifications.take();
        self.connection = Some(Connection::Bluetooth(connection));
        self.reconnect_requested = false;
        Ok(())
    }

    async fn initialize_connection(&mut self) {
        self.wired_host_link.reset();
        self.computer_target_links = false;
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

    fn snapshot(&self) -> CompanionSnapshot {
        let (connection_kind, connection_label) = match self.connection.as_ref() {
            Some(Connection::Usb(connection)) => {
                (NativeConnectionKind::Usb, connection.label().to_owned())
            }
            Some(Connection::Bluetooth(_)) => (
                NativeConnectionKind::Bluetooth,
                "Bluetooth · HIDShift".into(),
            ),
            None => (NativeConnectionKind::Searching, String::new()),
        };
        CompanionSnapshot {
            revision: self.snapshot_revision,
            device_revision: self.device_revision,
            connection_kind,
            connection_label,
            computer_name: self.computer_name.clone(),
            local_ble_host: self.local_ble_host.map(|host| host.0),
            local_wired: self.local_wired(),
            notifications_enabled: self.preferences.notifications_enabled,
            notification_prompt_seen: self.preferences.notification_prompt_seen,
            autostart_enabled: self.autostart_enabled,
        }
    }

    fn publish_snapshot(&mut self) {
        self.snapshot_revision = self.snapshot_revision.wrapping_add(1);
        let snapshot = self.snapshot();
        let _ = self.app.emit("hidshift://companion-state", &snapshot);
        crate::native_tray::update(
            &self.app,
            crate::native_tray::TraySnapshot {
                connection_label: self.connection.as_ref().map(Connection::label).as_deref(),
                status: self.status,
                output: self.output_status,
                host_names: &self.host_names,
                local_ble_host: self.local_ble_host,
                local_wired: self.local_wired(),
                computer_name: &self.computer_name,
            },
        );
    }

    fn local_wired(&self) -> bool {
        self.local_wired_present
            && self.output_status.is_some_and(|status| {
                status.wired_ready
                    && status.effective_presentation == ManagementUsbPresentationKind::Fallback
            })
    }

    fn local_computer_identity(&self) -> LocalComputerIdentity {
        LocalComputerIdentity {
            ble_host: self.local_ble_host,
            wired: self.local_wired(),
        }
    }

    fn start_maintenance_probes(&mut self) {
        if matches!(self.connection, Some(Connection::Bluetooth(_))) && self.usb_probe.is_none() {
            self.usb_probe = Some(tokio::task::spawn_blocking(NativeHid::connect));
        }
        if self.dual_s3 && self.wired_probe.is_none() {
            self.wired_probe = Some(tokio::task::spawn_blocking(standard_wired_present));
        }
        if self.usb_connected()
            && self.local_ble_host.is_none()
            && self.ble_identity_probe.is_none()
            && tokio::time::Instant::now() >= self.identity_probe_due
        {
            self.identity_probe_due =
                tokio::time::Instant::now() + core::time::Duration::from_secs(30);
            self.ble_identity_probe = Some(tokio::spawn(NativeBluetooth::probe_local_host()));
        }
    }

    async fn promote_usb(&mut self, mut connection: NativeHid) {
        if self.current_reply.is_some() || !self.queued.is_empty() {
            return;
        }
        self.hid_events = connection.take_events();
        self.connection = Some(Connection::Usb(connection));
        self.notifications = None;
        self.session.disconnected();
        self.reconnect_requested = false;
        self.initialize_connection().await;
        self.publish_snapshot();
    }

    fn set_local_ble_host(&mut self, host: HostId) {
        if self.local_ble_host == Some(host) {
            return;
        }
        self.local_ble_host = Some(host);
        self.session.set_local_host(Some(host));
        self.host_name_write_attempted = None;
        self.maybe_write_local_host_name();
        self.maybe_register_wired_host_link();
        self.reconcile_local_route();
        self.publish_snapshot();
    }

    fn observe_local_host_registration(&mut self, status: ManagementStatus) {
        let previous = self.local_ble_host;
        self.local_ble_host = retain_registered_local_host(status, self.local_ble_host);
        let Some(forgotten) = previous.filter(|_| self.local_ble_host.is_none()) else {
            return;
        };
        self.session.set_local_host(None);
        self.host_name_write_attempted = None;
        self.wired_host_link.reset();
        if let Some(index) = forgotten
            .0
            .checked_sub(1)
            .map(usize::from)
            .filter(|index| *index < self.host_names.len())
        {
            self.host_names[index] = None;
            self.host_name_sources[index] = 0;
            self.host_info_loaded[index] = false;
        }
        self.identity_probe_due = tokio::time::Instant::now();
    }

    fn observe_requested_selection(&mut self, command: ManagementCommand) {
        let target = match command {
            ManagementCommand::SelectHost(host) => Some(ManagementOutputTarget::Ble(host)),
            ManagementCommand::SelectOutputTarget(target) => Some(target),
            _ => None,
        };
        let Some(target) = target else {
            return;
        };
        let belongs_here =
            target_belongs_to_local_computer(target, Some(self.local_computer_identity()));
        if belongs_here {
            self.route_policy.select_this_computer();
        } else {
            self.route_policy.observe_external_selection(
                target,
                self.local_ble_host,
                self.local_wired(),
            );
        }
    }

    async fn select_this_computer(&mut self) {
        let route = if self.local_wired() {
            Some(DestinationRoute::Wired)
        } else {
            self.local_ble_host.map(DestinationRoute::Ble)
        };
        let Some(route) = route else { return };
        self.route_policy.select_this_computer();
        self.queue_internal(select_destination_command(route, self.dual_s3))
            .await;
    }

    async fn select_destination(&mut self, route: DestinationRoute) {
        let belongs_here =
            target_belongs_to_local_computer(route.target(), Some(self.local_computer_identity()));
        if belongs_here {
            self.route_policy.select_this_computer();
        } else {
            self.route_policy.select_other();
        }
        self.queue_internal(select_destination_command(route, self.dual_s3))
            .await;
    }

    fn maybe_write_local_host_name(&mut self) {
        let Some(host) = self.local_ble_host else {
            return;
        };
        let Some(index) = host
            .0
            .checked_sub(1)
            .map(usize::from)
            .filter(|index| *index < self.host_name_sources.len())
        else {
            return;
        };
        if !self.host_info_loaded[index]
            || self.host_name_sources[index] == 2
            || self.host_name_write_attempted == Some(host)
        {
            return;
        }
        self.host_name_write_attempted = Some(host);
        if let Some(name) = firmware_host_name(&self.computer_name) {
            let (send, _receive) = oneshot::channel();
            self.queued.push_back((
                ManagementCommand::SetHostName {
                    host_id: host,
                    name,
                },
                send,
            ));
        }
    }

    fn maybe_register_wired_host_link(&mut self) {
        let Some(host) = self.wired_host_link.next(
            self.computer_target_links,
            self.local_wired_present,
            self.local_ble_host,
        ) else {
            return;
        };
        let (send, _receive) = oneshot::channel();
        self.queued.push_back((
            ManagementCommand::SetWiredHostLink {
                ble_host: Some(host),
            },
            send,
        ));
    }

    fn reconcile_local_route(&mut self) {
        if self.route_switch_pending {
            return;
        }
        let Some(output) = self.output_status else {
            return;
        };
        let local_ble_ready = self.local_ble_host.is_some_and(|host| {
            host.0
                .checked_sub(1)
                .is_some_and(|index| output.ready_ble_mask & (1 << index) != 0)
        });
        let decision = self.route_policy.reconcile(
            output.selected,
            output.active,
            self.local_ble_host,
            self.local_wired(),
            local_ble_ready,
        );
        let RouteDecision::Switch {
            route,
            notify_fallback,
        } = decision
        else {
            return;
        };
        self.route_switch_pending = true;
        self.auto_notification = Some(if notify_fallback {
            AutoRouteNotification::FallbackToBluetooth
        } else {
            AutoRouteNotification::Silent
        });
        let (send, _receive) = oneshot::channel();
        self.queued
            .push_back((select_destination_command(route, self.dual_s3), send));
    }

    async fn enqueue(&mut self, command: ManagementCommand, reply: Reply) {
        self.observe_requested_selection(command);
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
            self.management_event(&notification.value).await;
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

    async fn hid_event(&mut self, event: NativeHidEvent) {
        match event {
            NativeHidEvent::Management(bytes) => self.management_event(&bytes).await,
            NativeHidEvent::Disconnected(error) => {
                eprintln!("USB HID management connection closed: {error}");
                self.disconnect();
            }
        }
    }

    async fn management_event(&mut self, bytes: &[u8]) {
        if !self.session.accept_event(bytes).unwrap_or(false) {
            return;
        }
        if self.dual_s3 {
            self.queue_internal(ManagementCommand::GetOutputTargetStatus)
                .await;
        } else {
            self.queue_internal(ManagementCommand::GetStatus).await;
        }
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
            ManagementResponsePayload::ClientSession(ManagementClientSession {
                host_id: Some(host),
            }) => self.set_local_ble_host(host),
            ManagementResponsePayload::ClientSession(ManagementClientSession { host_id: None }) => {
            }
            ManagementResponsePayload::Status(status) => {
                self.observe_local_host_registration(status);
                if self.status != Some(status) {
                    self.device_revision = self.device_revision.wrapping_add(1);
                }
                self.status = Some(status);
                if !self.dual_s3 {
                    let notification = self.session.observe_status(status);
                    self.notify_target_change(notification);
                }
                self.publish_snapshot();
            }
            ManagementResponsePayload::OutputTargetStatus(status) => {
                if self.output_status != Some(status) {
                    self.device_revision = self.device_revision.wrapping_add(1);
                }
                self.output_status = Some(status);
                self.route_switch_pending = false;
                let notification = self.session.observe_output_status(status);
                self.notify_target_change(notification);
                self.reconcile_local_route();
                self.publish_snapshot();
            }
            ManagementResponsePayload::Schema(schema) => {
                self.dual_s3 =
                    schema.capabilities & hidshift::MANAGEMENT_CAPABILITY_DUAL_S3_WIRED != 0;
                self.computer_target_links = schema.capabilities
                    & hidshift::MANAGEMENT_CAPABILITY_COMPUTER_TARGET_LINKS
                    != 0;
                self.maybe_register_wired_host_link();
            }
            ManagementResponsePayload::HostInfo(info) => {
                if let Some(index) = info
                    .host_id
                    .0
                    .checked_sub(1)
                    .map(usize::from)
                    .filter(|index| *index < 4)
                {
                    let name = core::str::from_utf8(info.name.as_bytes())
                        .ok()
                        .map(str::to_owned);
                    if self.host_names[index] != name
                        || self.host_name_sources[index] != info.name_source
                    {
                        self.device_revision = self.device_revision.wrapping_add(1);
                    }
                    self.host_names[index] = name;
                    self.host_name_sources[index] = info.name_source;
                    self.host_info_loaded[index] = true;
                    self.maybe_write_local_host_name();
                    self.publish_snapshot();
                }
            }
            _ => {}
        }
    }

    fn notify_target_change(&mut self, notification: Option<ActiveTargetNotification>) {
        let Some(notification) = notification else {
            return;
        };
        if let Some(auto) = self.auto_notification.take() {
            if auto == AutoRouteNotification::FallbackToBluetooth {
                self.show_notification(
                    "有線接続が切れたため、このPCへBluetoothで切り替えました".into(),
                );
            }
            return;
        }
        let body = target_change_notification_body(
            notification,
            Some(self.local_computer_identity()),
            &self.computer_name,
            |host| self.host_label(host),
        );
        self.show_notification(body);
    }

    fn show_notification(&self, body: String) {
        if !self.preferences.notifications_enabled {
            return;
        }
        self.notifier.show(body.clone());
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
        self.hid_events = None;
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
        self.status = None;
        self.output_status = None;
        self.route_switch_pending = false;
        self.auto_notification = None;
        self.wired_host_link.reset();
        self.publish_snapshot();
    }
}

fn target_change_notification_body(
    notification: ActiveTargetNotification,
    local: Option<LocalComputerIdentity>,
    computer_name: &str,
    host_label: impl FnOnce(HostId) -> String,
) -> String {
    let local_label = || {
        if computer_name.is_empty() {
            "このPC".to_owned()
        } else {
            format!("このPC · {computer_name}")
        }
    };
    let destination = match notification {
        ActiveTargetNotification::ThisComputer => local_label(),
        ActiveTargetNotification::OtherComputer(host) => host_label(host),
        ActiveTargetNotification::Wired
            if target_belongs_to_local_computer(ManagementOutputTarget::Wired, local) =>
        {
            local_label()
        }
        ActiveTargetNotification::Wired => "有線USB".to_owned(),
    };
    format!("入力先を「{destination}」へ切り替えました")
}

fn parse_uuid(value: &str) -> Result<Uuid, String> {
    Uuid::parse_str(value).map_err(|error| error.to_string())
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

    #[test]
    fn wired_and_bluetooth_notifications_use_the_same_local_computer_name() {
        let local = Some(LocalComputerIdentity {
            ble_host: Some(HostId(1)),
            wired: true,
        });
        let local_bluetooth = target_change_notification_body(
            ActiveTargetNotification::ThisComputer,
            local,
            "example-desktop",
            |_| unreachable!(),
        );
        let local_wired = target_change_notification_body(
            ActiveTargetNotification::Wired,
            local,
            "example-desktop",
            |_| unreachable!(),
        );

        assert_eq!(local_wired, local_bluetooth);
        assert_eq!(
            local_wired,
            "入力先を「このPC · example-desktop」へ切り替えました"
        );
    }

    #[test]
    fn remote_wired_route_is_not_mislabeled_as_this_computer() {
        assert_eq!(
            target_change_notification_body(
                ActiveTargetNotification::Wired,
                Some(LocalComputerIdentity {
                    ble_host: Some(HostId(2)),
                    wired: false,
                }),
                "remote-pc",
                |_| unreachable!(),
            ),
            "入力先を「有線USB」へ切り替えました"
        );
    }
}
