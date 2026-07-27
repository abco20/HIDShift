use std::rc::Rc;

use hidshift::{
    HostId, MANAGEMENT_CAPABILITY_DUAL_S3_WIRED, ManagementCommand, ManagementDiagnostics,
    ManagementHistoryEvent, ManagementHostTiming, ManagementMirrorCandidate,
    ManagementOutputTargetStatus, ManagementResponse, ManagementResponsePayload, ManagementResult,
    ManagementSchema, ManagementStatus, MirrorCandidateId, SETTING_DESCRIPTORS, SettingDescriptor,
    SettingScope, SettingTarget,
};
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen_futures::spawn_local;

use crate::browser_client::{BrowserClient, BrowserClientError};
use crate::transport::BrowserTransport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Page {
    Home,
    Destinations,
    Inputs,
    Settings,
    Support,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Locale {
    Ja,
    En,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct UsbDeviceView {
    pub index: u8,
    pub device_id: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub flags: u8,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SettingView {
    pub descriptor: &'static SettingDescriptor,
    pub target: SettingTarget,
    pub value: i32,
}

#[derive(Clone)]
pub(crate) struct AppState {
    pub client: SendWrapper<Rc<BrowserClient>>,
    pub page: RwSignal<Page>,
    pub locale: RwSignal<Locale>,
    pub theme: RwSignal<Theme>,
    pub status: RwSignal<Option<ManagementStatus>>,
    pub names: RwSignal<[String; 4]>,
    pub name_sources: RwSignal<[u8; 4]>,
    pub timings: RwSignal<[Option<ManagementHostTiming>; 4]>,
    pub usb_devices: RwSignal<Vec<UsbDeviceView>>,
    pub diagnostics: RwSignal<Option<ManagementDiagnostics>>,
    pub schema: RwSignal<Option<ManagementSchema>>,
    pub history: RwSignal<Vec<ManagementHistoryEvent>>,
    pub settings: RwSignal<Vec<SettingView>>,
    pub output_status: RwSignal<Option<ManagementOutputTargetStatus>>,
    pub mirror_candidates: RwSignal<Vec<ManagementMirrorCandidate>>,
    pub connection: RwSignal<String>,
    pub connected: RwSignal<bool>,
    pub busy: RwSignal<bool>,
    pub message: RwSignal<String>,
    pub is_error: RwSignal<bool>,
    pub undo: RwSignal<Option<ManagementCommand>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            client: SendWrapper::new(BrowserClient::new()),
            page: RwSignal::new(Page::Home),
            locale: RwSignal::new(initial_locale()),
            theme: RwSignal::new(initial_theme()),
            status: RwSignal::new(None),
            names: RwSignal::new(core::array::from_fn(|_| String::new())),
            name_sources: RwSignal::new([0; 4]),
            timings: RwSignal::new([None; 4]),
            usb_devices: RwSignal::new(Vec::new()),
            diagnostics: RwSignal::new(None),
            schema: RwSignal::new(None),
            history: RwSignal::new(Vec::new()),
            settings: RwSignal::new(Vec::new()),
            output_status: RwSignal::new(None),
            mirror_candidates: RwSignal::new(Vec::new()),
            connection: RwSignal::new(String::new()),
            connected: RwSignal::new(false),
            busy: RwSignal::new(false),
            message: RwSignal::new(String::new()),
            is_error: RwSignal::new(false),
            undo: RwSignal::new(None),
        }
    }

    pub fn navigate(&self, page: Page) {
        self.page.set(page);
        self.refresh();
    }

    pub fn refresh(&self) {
        if !self.connected.get_untracked() || self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        let state = self.clone();
        spawn_local(async move {
            let result = state.load_page(state.page.get_untracked()).await;
            state.finish(result, None);
        });
    }

    pub fn send(&self, command: ManagementCommand) {
        if !self.connected.get_untracked() || self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.message.set(String::new());
        if let ManagementCommand::SetSetting { id, target, .. } = command {
            self.undo.set(
                self.settings
                    .get_untracked()
                    .iter()
                    .find(|setting| setting.descriptor.id == id && setting.target == target)
                    .map(|setting| ManagementCommand::SetSetting {
                        id,
                        target,
                        value: setting.value,
                    }),
            );
        } else {
            self.undo.set(None);
        }
        let state = self.clone();
        spawn_local(async move {
            let result = async {
                let response = state.client.request(command).await?;
                ensure_ok(response)?;
                if let ManagementCommand::SetSetting { id, target, value } = command {
                    state.settings.update(|settings| {
                        if let Some(setting) = settings
                            .iter_mut()
                            .find(|setting| setting.descriptor.id == id && setting.target == target)
                        {
                            setting.value = value;
                        }
                    });
                    Ok(())
                } else {
                    state.load_page(state.page.get_untracked()).await
                }
            }
            .await;
            state.finish(result, Some(("変更を保存しました", "Saved")));
        });
    }

    pub fn undo(&self) {
        if let Some(command) = self.undo.get_untracked() {
            self.undo.set(None);
            self.send(command);
        }
    }

    pub fn connect(&self, bluetooth: bool) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        self.is_error.set(false);
        self.message.set(match self.locale.get_untracked() {
            Locale::Ja => "接続しています…".into(),
            Locale::En => "Connecting…".into(),
        });
        let state = self.clone();
        let bytes_client = self.client.clone();
        let disconnect_state = self.clone();
        spawn_local(async move {
            let on_bytes = Rc::new(move |bytes: &[u8]| bytes_client.receive(bytes));
            let on_disconnect = Rc::new(move |reason: String| {
                disconnect_state.client.detach();
                disconnect_state.connected.set(false);
                disconnect_state.status.set(None);
                disconnect_state.connection.set(String::new());
                disconnect_state.message.set(reason);
                disconnect_state.is_error.set(true);
                disconnect_state.busy.set(false);
            });
            let transport = if bluetooth {
                BrowserTransport::connect_bluetooth(on_bytes, on_disconnect).await
            } else {
                BrowserTransport::connect_serial(on_bytes, on_disconnect).await
            };
            match transport {
                Ok(transport) => {
                    state.connection.set(transport.label());
                    state.client.attach(transport);
                    state.connected.set(true);
                    let result = state.load_page(Page::Home).await;
                    state.finish(result, Some(("接続しました", "Connected")));
                }
                Err(error) => state.finish(
                    Err(BrowserClientError::Transport(error)),
                    Option::<(&str, &str)>::None,
                ),
            }
        });
    }

    pub fn set_locale(&self, locale: Locale) {
        self.locale.set(locale);
        if let Some(storage) = local_storage() {
            let _ = storage.set_item(
                "hidshift-language",
                match locale {
                    Locale::Ja => "ja",
                    Locale::En => "en",
                },
            );
        }
    }

    pub fn set_theme(&self, theme: Theme) {
        self.theme.set(theme);
        let value = match theme {
            Theme::System => None,
            Theme::Light => Some("light"),
            Theme::Dark => Some("dark"),
        };
        if let Some(document) = web_sys::window().and_then(|window| window.document())
            && let Some(root) = document.document_element()
        {
            match value {
                Some(value) => {
                    let _ = root.set_attribute("data-theme", value);
                }
                None => {
                    let _ = root.remove_attribute("data-theme");
                }
            }
        }
        if let Some(storage) = local_storage() {
            match value {
                Some(value) => {
                    let _ = storage.set_item("hidshift-theme", value);
                }
                None => {
                    let _ = storage.remove_item("hidshift-theme");
                }
            }
        }
    }

    async fn load_page(&self, page: Page) -> Result<(), BrowserClientError> {
        match page {
            Page::Home => self.load_summary().await,
            Page::Destinations => {
                self.load_summary().await?;
                self.load_destination_details().await
            }
            Page::Inputs => self.load_inputs().await,
            Page::Settings => self.load_settings().await,
            Page::Support => self.load_support().await,
        }
    }

    async fn load_status(&self) -> Result<ManagementStatus, BrowserClientError> {
        let response = self.client.request(ManagementCommand::GetStatus).await?;
        ensure_ok(response)?;
        let ManagementResponsePayload::Status(status) = response.payload else {
            return Err(BrowserClientError::Protocol(
                "status payload missing".into(),
            ));
        };
        self.status.set(Some(status));
        Ok(status)
    }

    async fn load_summary(&self) -> Result<(), BrowserClientError> {
        let status = self.load_status().await?;
        for index in 0..status.host_count.min(4) as usize {
            if !status.hosts[index].known {
                continue;
            }
            let response = self
                .client
                .request(ManagementCommand::GetHostInfo(HostId((index + 1) as u8)))
                .await?;
            ensure_ok(response)?;
            if let ManagementResponsePayload::HostInfo(info) = response.payload {
                self.names.update(|names| {
                    names[index] = String::from_utf8_lossy(info.name.as_bytes()).into_owned()
                });
                self.name_sources
                    .update(|sources| sources[index] = info.name_source);
            }
        }
        let schema = self.load_schema().await?;
        if schema.capabilities & MANAGEMENT_CAPABILITY_DUAL_S3_WIRED != 0 {
            let response = self
                .client
                .request(ManagementCommand::GetOutputTargetStatus)
                .await?;
            ensure_ok(response)?;
            if let ManagementResponsePayload::OutputTargetStatus(output) = response.payload {
                self.output_status.set(Some(output));
            }
        } else {
            self.output_status.set(None);
        }
        Ok(())
    }

    async fn load_destination_details(&self) -> Result<(), BrowserClientError> {
        let Some(status) = self.status.get_untracked() else {
            return Ok(());
        };
        for index in 0..status.host_count.min(4) as usize {
            if !status.hosts[index].known {
                continue;
            }
            let response = self
                .client
                .request(ManagementCommand::GetHostTiming(HostId((index + 1) as u8)))
                .await?;
            ensure_ok(response)?;
            if let ManagementResponsePayload::HostTiming(timing) = response.payload {
                self.timings.update(|timings| timings[index] = Some(timing));
            }
        }
        Ok(())
    }

    async fn load_inputs(&self) -> Result<(), BrowserClientError> {
        let status = self.load_status().await?;
        let mut devices = Vec::new();
        for index in 0..status.usb.device_count {
            let mut offset = 0;
            let mut name = Vec::new();
            let mut metadata = None;
            loop {
                let response = self
                    .client
                    .request(ManagementCommand::GetUsbDevice {
                        index,
                        name_offset: offset,
                    })
                    .await?;
                ensure_ok(response)?;
                let ManagementResponsePayload::UsbDevice(device) = response.payload else {
                    break;
                };
                metadata.get_or_insert(device);
                name.extend_from_slice(device.name_chunk());
                offset = offset.saturating_add(device.name_chunk_len);
                if offset >= device.name_len || device.name_chunk_len == 0 {
                    break;
                }
            }
            if let Some(device) = metadata {
                devices.push(UsbDeviceView {
                    index,
                    device_id: device.device_id,
                    vendor_id: device.vendor_id,
                    product_id: device.product_id,
                    flags: device.flags,
                    name: String::from_utf8_lossy(&name).into_owned(),
                });
            }
        }
        self.usb_devices.set(devices);
        let schema = self.load_schema().await?;
        if schema.capabilities & MANAGEMENT_CAPABILITY_DUAL_S3_WIRED != 0 {
            let mut candidates = Vec::new();
            for candidate in 0..4 {
                let response = self
                    .client
                    .request(ManagementCommand::GetMirrorCandidate(MirrorCandidateId(
                        candidate,
                    )))
                    .await?;
                if response.result == ManagementResult::NotFound {
                    continue;
                }
                ensure_ok(response)?;
                if let ManagementResponsePayload::MirrorCandidate(candidate) = response.payload {
                    candidates.push(candidate);
                }
            }
            self.mirror_candidates.set(candidates);
        }
        Ok(())
    }

    async fn load_settings(&self) -> Result<(), BrowserClientError> {
        self.load_schema().await?;
        let mut values = Vec::new();
        for descriptor in SETTING_DESCRIPTORS {
            match descriptor.scope {
                SettingScope::Global => {
                    if let Some(value) = self.get_setting(descriptor, SettingTarget::Global).await?
                    {
                        values.push(value);
                    }
                }
                SettingScope::Host => {
                    for slot in 1..=4 {
                        if let Some(value) = self
                            .get_setting(descriptor, SettingTarget::Host(HostId(slot)))
                            .await?
                        {
                            values.push(value);
                        }
                    }
                }
            }
        }
        self.settings.set(values);
        Ok(())
    }

    async fn load_support(&self) -> Result<(), BrowserClientError> {
        let response = self
            .client
            .request(ManagementCommand::GetDiagnostics)
            .await?;
        ensure_ok(response)?;
        if let ManagementResponsePayload::Diagnostics(value) = response.payload {
            self.diagnostics.set(Some(value));
        }
        self.load_schema().await?;
        let mut events = Vec::new();
        for index in 0..16 {
            let response = self
                .client
                .request(ManagementCommand::GetHistory { index })
                .await?;
            if response.result == ManagementResult::NotFound {
                break;
            }
            ensure_ok(response)?;
            if let ManagementResponsePayload::History(event) = response.payload {
                events.push(event);
            } else {
                break;
            }
        }
        self.history.set(events);
        Ok(())
    }

    async fn load_schema(&self) -> Result<ManagementSchema, BrowserClientError> {
        if let Some(schema) = self.schema.get_untracked() {
            return Ok(schema);
        }
        let response = self.client.request(ManagementCommand::GetSchema).await?;
        ensure_ok(response)?;
        let ManagementResponsePayload::Schema(schema) = response.payload else {
            return Err(BrowserClientError::Protocol(
                "schema payload missing".into(),
            ));
        };
        if schema.version != hidshift::SETTINGS_SCHEMA_VERSION
            || schema.setting_count as usize != hidshift::SETTING_COUNT
            || schema.hash != hidshift::SETTINGS_SCHEMA_HASH
        {
            return Err(BrowserClientError::Protocol(
                "firmware and Web UI schemas do not match".into(),
            ));
        }
        self.schema.set(Some(schema));
        Ok(schema)
    }

    async fn get_setting(
        &self,
        descriptor: &'static SettingDescriptor,
        target: SettingTarget,
    ) -> Result<Option<SettingView>, BrowserClientError> {
        let response = self
            .client
            .request(ManagementCommand::GetSetting {
                id: descriptor.id,
                target,
            })
            .await?;
        ensure_ok(response)?;
        Ok(match response.payload {
            ManagementResponsePayload::Setting(setting) => Some(SettingView {
                descriptor,
                target,
                value: setting.value,
            }),
            _ => None,
        })
    }

    fn finish(&self, result: Result<(), BrowserClientError>, success: Option<(&str, &str)>) {
        match result {
            Ok(()) => {
                self.is_error.set(false);
                if let Some((ja, en)) = success {
                    self.message.set(match self.locale.get_untracked() {
                        Locale::Ja => ja.into(),
                        Locale::En => en.into(),
                    });
                } else {
                    self.message.set(String::new());
                }
            }
            Err(error) => {
                self.is_error.set(true);
                self.message.set(client_error_message(error));
            }
        }
        self.busy.set(false);
    }
}

pub(crate) fn ensure_ok(response: ManagementResponse) -> Result<(), BrowserClientError> {
    if response.result == ManagementResult::Ok {
        Ok(())
    } else {
        Err(BrowserClientError::Protocol(format!(
            "{:?}",
            response.result
        )))
    }
}

fn initial_locale() -> Locale {
    if let Some(storage) = local_storage()
        && let Ok(Some(value)) = storage.get_item("hidshift-language")
    {
        return if value == "en" {
            Locale::En
        } else {
            Locale::Ja
        };
    }
    let language = web_sys::window()
        .and_then(|window| window.navigator().language())
        .unwrap_or_default();
    if language.starts_with("ja") {
        Locale::Ja
    } else {
        Locale::En
    }
}

fn initial_theme() -> Theme {
    let value = local_storage()
        .and_then(|storage| storage.get_item("hidshift-theme").ok().flatten())
        .unwrap_or_default();
    match value.as_str() {
        "light" => Theme::Light,
        "dark" => Theme::Dark,
        _ => Theme::System,
    }
}

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn client_error_message(error: BrowserClientError) -> String {
    match error {
        BrowserClientError::Busy => "Another request is still running".into(),
        BrowserClientError::Disconnected => "HIDShift disconnected".into(),
        BrowserClientError::Transport(error) => error,
        BrowserClientError::Protocol(error) => format!("Invalid device response: {error}"),
        BrowserClientError::Timeout => "HIDShift did not respond in time".into(),
    }
}
