use std::rc::Rc;

use hidshift::{
    HostId, ManagementCommand, ManagementDiagnostics, ManagementHistoryEvent, ManagementHostName,
    ManagementHostStatus, ManagementHostTiming, ManagementResponse, ManagementResponsePayload,
    ManagementResult, ManagementSchema, ManagementStatus, SETTING_DESCRIPTORS, SettingDescriptor,
    SettingScope, SettingTarget,
};
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::browser_client::{BrowserClient, BrowserClientError};
use crate::settings_ui::SettingsPanel;
use crate::transport::BrowserTransport;

#[derive(Clone, Debug, Eq, PartialEq)]
struct UsbDeviceView {
    index: u8,
    device_id: u8,
    vendor_id: u16,
    product_id: u16,
    flags: u8,
    name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SettingView {
    pub(crate) descriptor: &'static SettingDescriptor,
    pub(crate) target: SettingTarget,
    pub(crate) value: i32,
}

#[component]
pub fn App() -> impl IntoView {
    let client = BrowserClient::new();
    let status = RwSignal::new(None::<ManagementStatus>);
    let names = RwSignal::new(core::array::from_fn::<_, 4, _>(|_| String::new()));
    let name_sources = RwSignal::new([0u8; 4]);
    let timings = RwSignal::new([None::<ManagementHostTiming>; 4]);
    let usb_devices = RwSignal::new(Vec::<UsbDeviceView>::new());
    let diagnostics = RwSignal::new(None::<ManagementDiagnostics>);
    let schema = RwSignal::new(None::<ManagementSchema>);
    let history = RwSignal::new(Vec::<ManagementHistoryEvent>::new());
    let settings = RwSignal::new(Vec::<SettingView>::new());
    let connection = RwSignal::new("未接続".to_owned());
    let connected = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let message = RwSignal::new(String::new());
    let is_error = RwSignal::new(false);

    let send: SendWrapper<Rc<dyn Fn(ManagementCommand)>> = {
        let client = client.clone();
        SendWrapper::new(Rc::new(move |command| {
            if busy.get_untracked() {
                return;
            }
            busy.set(true);
            message.set("処理中…".into());
            is_error.set(false);
            let client = client.clone();
            spawn_local(async move {
                let result = async {
                    let response = client.request(command).await?;
                    ensure_ok(response)?;
                    refresh_all(
                        &client,
                        status,
                        names,
                        name_sources,
                        timings,
                        usb_devices,
                        diagnostics,
                        schema,
                        history,
                        settings,
                    )
                    .await
                }
                .await;
                match result {
                    Ok(()) => message.set("状態を更新しました".into()),
                    Err(error) => {
                        message.set(client_error_message(error));
                        is_error.set(true);
                    }
                }
                busy.set(false);
            });
        }))
    };

    let connect_ble = connect_action(
        client.clone(),
        true,
        status,
        connection,
        connected,
        busy,
        message,
        is_error,
        send.clone(),
    );
    let connect_serial = connect_action(
        client.clone(),
        false,
        status,
        connection,
        connected,
        busy,
        message,
        is_error,
        send.clone(),
    );
    let refresh = {
        let send = send.clone();
        move |_| send(ManagementCommand::GetStatus)
    };
    let cancel_pairing = {
        let send = send.clone();
        move |_| send(ManagementCommand::CancelPairing)
    };
    let copy_logs = move |_| {
        let text = history
            .get_untracked()
            .iter()
            .map(format_history)
            .collect::<Vec<_>>()
            .join("\n");
        spawn_local(async move {
            if let Some(window) = web_sys::window() {
                let _ = JsFuture::from(window.navigator().clipboard().write_text(&text)).await;
            }
        });
    };
    let slot_send = send.clone();
    let setting_send = send.clone();

    view! {
        <main>
            <header class="app-header">
                <div class="brand-mark" aria-hidden="true">"H"</div>
                <div class="brand-copy">
                    <p class="eyebrow">"HIDShift"</p>
                    <h1>"デバイスマネージャー"</h1>
                    <p>"USB機器とBluetooth接続先を、ひとつの画面で管理します。"</p>
                </div>
            </header>

            <section class:online=move || connected.get() class="connection-panel" aria-label="HIDShiftへの接続">
                <div class="connection-copy">
                    <div class="connection-label"><span class="status-dot"></span>{move || if connected.get() { "HIDShiftに接続済み" } else { "HIDShiftに接続してください" }}</div>
                    <strong>{move || connection.get()}</strong>
                    <p>{move || if connected.get() { "機器の状態は取得済みです。変更内容はHIDShift本体に保存されます。" } else { "普段はBluetooth、初期設定や復旧時はUSBケーブルでの接続がおすすめです。" }}</p>
                </div>
                <div class="connect-actions">
                    <button class="primary" on:click=connect_ble disabled=move || busy.get()><span aria-hidden="true">"ᛒ"</span>"Bluetoothで接続"</button>
                    <button class="secondary" on:click=connect_serial disabled=move || busy.get()><span aria-hidden="true">"⌁"</span>"USBケーブルで接続"</button>
                    <button class="icon-button" title="状態を更新" aria-label="状態を更新" on:click=refresh disabled=move || !connected.get() || busy.get()>"↻"</button>
                </div>
                <p class="connection-help">"USB接続ではHIDShiftに接続したシリアルポートを選んでください。接続候補に出ない場合は、データ通信対応のUSBケーブルか確認してください。"</p>
            </section>

            <p class:error=move || is_error.get() class:visible=move || !message.get().is_empty() class="message" role="status">
                <span aria-hidden="true">{move || if is_error.get() { "!" } else { "✓" }}</span>{move || message.get()}
            </p>

            <section class="summary" aria-label="現在の状態">
                <article class="summary-primary"><span>"現在の接続先"</span><strong>{move || display_host(status.get().and_then(|s| s.active_host), &names.get())}</strong><small>{move || status.get().and_then(|s| s.active_host).map(|_| "キーボード・マウス入力の送信先").unwrap_or("接続後に選択できます")}</small></article>
                <article><span>"USB入力機器"</span><strong>{move || status.get().map(|s| format!("{} 台", s.usb.device_count)).unwrap_or_else(|| "—".into())}</strong><small>{move || status.get().map(|s| format!("HIDインターフェース {}個", s.usb.interface_count)).unwrap_or_else(|| "未取得".into())}</small></article>
                <article><span>"ペアリング"</span><strong>{move || status.get().and_then(|s| s.pairing_host).map(|host| display_host(Some(host), &names.get())).unwrap_or_else(|| "待機中".into())}</strong><small>"新しいPCやスマートフォンを追加"</small></article>
                <article><span>"システム"</span><strong>{move || if connected.get() { "正常稼働" } else { "未確認" }}</strong><small>{move || diagnostics.get().map(|d| format!("稼働 {} · BLE切断 {}回", format_duration(d.uptime_seconds), d.ble_disconnect_count)).unwrap_or_else(|| "接続すると状態を確認できます".into())}</small></article>
            </section>

            <section class="content-section">
                <div class="section-title">
                    <div><p class="section-kicker">"OUTPUTS"</p><h2>"接続先"</h2><p>"入力を送りたい機器を選ぶか、新しい機器を追加します。"</p></div>
                    <button class="quiet" on:click=cancel_pairing disabled=move || busy.get() || status.get().and_then(|s| s.pairing_host).is_none()>"ペアリングを中止"</button>
                </div>
                <div class="slots">
                    {move || status.get().map(|value| {
                        (0..value.host_count as usize).map(|index| {
                            slot_card(index, value, names.get()[index].clone(), name_sources.get()[index], timings.get()[index], busy, slot_send.clone())
                        }).collect_view()
                    })}
                </div>
            </section>

            <section class="content-section">
                <div class="section-title"><div><p class="section-kicker">"INPUTS"</p><h2>"接続中のUSB機器"</h2><p>"HIDShiftが認識しているキーボード、マウス、操作デバイスです。"</p></div></div>
                <div class="device-grid">
                    {move || if usb_devices.get().is_empty() {
                        view! { <div class="empty"><span aria-hidden="true">"⌨"</span><strong>"USB機器が見つかりません"</strong><p>"HIDShiftのUSB Hostポートに機器を接続してください。"</p></div> }.into_any()
                    } else {
                        usb_devices.get().into_iter().map(usb_device_card).collect_view().into_any()
                    }}
                </div>
            </section>

            <section class="advanced-section">
                <div class="section-title"><div><p class="section-kicker">"CUSTOMIZE & SUPPORT"</p><h2>"設定とサポート"</h2><p>"必要なときだけ開いて確認できます。"</p></div></div>
                <details open>
                    <summary><span><strong>"動作設定"</strong><small>"接続先ごとの感度や動作を変更"</small></span><span class="chevron">"⌄"</span></summary>
                    <div class="detail-body"><SettingsPanel settings busy send=setting_send.clone()/></div>
                </details>
                <details>
                    <summary><span><strong>"システム診断"</strong><small>"再起動・通信・保存状態を確認"</small></span><span class="chevron">"⌄"</span></summary>
                    <div class="detail-body">{move || diagnostics.get().map(diagnostics_view)}</div>
                </details>
                <details>
                    <summary><span><strong>"接続履歴とログ"</strong><small>"トラブル調査用のイベント記録"</small></span><span class="chevron">"⌄"</span></summary>
                    <div class="detail-body"><div class="log-toolbar"><span>"新しいイベントが先頭です"</span><button class="quiet compact" on:click=copy_logs>"ログをコピー"</button></div><pre class="logs">{move || history.get().iter().map(format_history).collect::<Vec<_>>().join("\n")}</pre></div>
                </details>
                <p class="firmware-version">{move || schema.get().map(|s| format!("Firmware {}.{}.{} · Schema {}", s.firmware_major, s.firmware_minor, s.firmware_patch, s.version)).unwrap_or_else(|| "Firmware情報は未取得です".into())}</p>
            </section>
        </main>
    }
}

#[allow(clippy::too_many_arguments)]
async fn refresh_all(
    client: &BrowserClient,
    status_signal: RwSignal<Option<ManagementStatus>>,
    names: RwSignal<[String; 4]>,
    name_sources: RwSignal<[u8; 4]>,
    timings: RwSignal<[Option<ManagementHostTiming>; 4]>,
    usb_devices: RwSignal<Vec<UsbDeviceView>>,
    diagnostics: RwSignal<Option<ManagementDiagnostics>>,
    schema_signal: RwSignal<Option<ManagementSchema>>,
    history: RwSignal<Vec<ManagementHistoryEvent>>,
    settings: RwSignal<Vec<SettingView>>,
) -> Result<(), BrowserClientError> {
    let response = client.request(ManagementCommand::GetStatus).await?;
    ensure_ok(response)?;
    let ManagementResponsePayload::Status(status) = response.payload else {
        return Err(BrowserClientError::Protocol(
            "status payload missing".into(),
        ));
    };
    status_signal.set(Some(status));
    for index in 0..status.host_count as usize {
        if !status.hosts[index].known {
            continue;
        }
        let response = client
            .request(ManagementCommand::GetHostInfo(HostId((index + 1) as u8)))
            .await?;
        ensure_ok(response)?;
        if let ManagementResponsePayload::HostInfo(info) = response.payload {
            names.update(|values| {
                values[index] = String::from_utf8_lossy(info.name.as_bytes()).into_owned()
            });
            name_sources.update(|values| values[index] = info.name_source);
        }
        let response = client
            .request(ManagementCommand::GetHostTiming(HostId((index + 1) as u8)))
            .await?;
        ensure_ok(response)?;
        if let ManagementResponsePayload::HostTiming(value) = response.payload {
            timings.update(|values| values[index] = Some(value));
        }
    }

    let mut devices = Vec::new();
    for index in 0..status.usb.device_count {
        let mut offset = 0u8;
        let mut name = Vec::new();
        let mut metadata = None;
        loop {
            let response = client
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
    usb_devices.set(devices);

    let response = client.request(ManagementCommand::GetDiagnostics).await?;
    ensure_ok(response)?;
    if let ManagementResponsePayload::Diagnostics(value) = response.payload {
        diagnostics.set(Some(value));
    }

    let mut events = Vec::new();
    for index in 0..16 {
        let response = client
            .request(ManagementCommand::GetHistory { index })
            .await?;
        ensure_ok(response)?;
        match response.payload {
            ManagementResponsePayload::History(event) => events.push(event),
            _ => break,
        }
    }
    history.set(events);

    let response = client.request(ManagementCommand::GetSchema).await?;
    ensure_ok(response)?;
    if let ManagementResponsePayload::Schema(value) = response.payload {
        if value.version != hidshift::SETTINGS_SCHEMA_VERSION
            || value.setting_count as usize != hidshift::SETTING_COUNT
            || value.hash != hidshift::SETTINGS_SCHEMA_HASH
        {
            return Err(BrowserClientError::Protocol(
                "firmwareとWeb UIの設定スキーマが一致しません".into(),
            ));
        }
        schema_signal.set(Some(value));
    }
    let mut values = Vec::new();
    for descriptor in SETTING_DESCRIPTORS {
        match descriptor.scope {
            SettingScope::Global => {
                if let Some(value) = get_setting(client, descriptor, SettingTarget::Global).await? {
                    values.push(value);
                }
            }
            SettingScope::Host => {
                for slot in 1..=4 {
                    if let Some(value) =
                        get_setting(client, descriptor, SettingTarget::Host(HostId(slot))).await?
                    {
                        values.push(value);
                    }
                }
            }
        }
    }
    settings.set(values);
    Ok(())
}

async fn get_setting(
    client: &BrowserClient,
    descriptor: &'static SettingDescriptor,
    target: SettingTarget,
) -> Result<Option<SettingView>, BrowserClientError> {
    let response = client
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

fn ensure_ok(response: ManagementResponse) -> Result<(), BrowserClientError> {
    if response.result == ManagementResult::Ok {
        Ok(())
    } else {
        Err(BrowserClientError::Protocol(
            result_message(response.result).into(),
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn connect_action(
    client: Rc<BrowserClient>,
    bluetooth: bool,
    status: RwSignal<Option<ManagementStatus>>,
    connection: RwSignal<String>,
    connected: RwSignal<bool>,
    busy: RwSignal<bool>,
    message: RwSignal<String>,
    is_error: RwSignal<bool>,
    send: SendWrapper<Rc<dyn Fn(ManagementCommand)>>,
) -> impl Fn(web_sys::MouseEvent) + 'static {
    move |_| {
        busy.set(true);
        message.set("接続しています…".into());
        is_error.set(false);
        let client_for_connect = client.clone();
        let client_for_bytes = client.clone();
        let client_for_disconnect = client.clone();
        let send = send.clone();
        spawn_local(async move {
            let on_bytes = Rc::new(move |bytes: &[u8]| client_for_bytes.receive(bytes));
            let on_disconnect = Rc::new(move |reason: String| {
                client_for_disconnect.detach();
                connected.set(false);
                status.set(None);
                connection.set("未接続".into());
                message.set(reason);
                is_error.set(true);
                busy.set(false);
            });
            let result = if bluetooth {
                BrowserTransport::connect_bluetooth(on_bytes, on_disconnect).await
            } else {
                BrowserTransport::connect_serial(on_bytes, on_disconnect).await
            };
            match result {
                Ok(transport) => {
                    connection.set(transport.label());
                    client_for_connect.attach(transport);
                    connected.set(true);
                    busy.set(false);
                    message.set("接続しました".into());
                    send(ManagementCommand::GetStatus);
                }
                Err(error) => {
                    busy.set(false);
                    message.set(error);
                    is_error.set(true);
                }
            }
        });
    }
}

fn slot_card(
    index: usize,
    status: ManagementStatus,
    name: String,
    name_source: u8,
    timing: Option<ManagementHostTiming>,
    busy: RwSignal<bool>,
    send: SendWrapper<Rc<dyn Fn(ManagementCommand)>>,
) -> impl IntoView {
    let host = HostId((index + 1) as u8);
    let flags = status.hosts[index];
    let active = status.active_host == Some(host);
    let pairing = status.pairing_host == Some(host);
    let select = {
        let send = send.clone();
        move |_| send(ManagementCommand::SelectHost(host))
    };
    let pair = {
        let send = send.clone();
        move |_| send(ManagementCommand::StartPairing(host))
    };
    let forget = {
        let send = send.clone();
        move |_| {
            if web_sys::window()
                .and_then(|window| {
                    window
                        .confirm_with_message(&format!(
                            "スロット {} のbondを削除しますか？",
                            host.0
                        ))
                        .ok()
                })
                .unwrap_or(false)
            {
                send(ManagementCommand::ForgetHost(host));
            }
        }
    };
    let rename = {
        let send = send.clone();
        let current_name = name.clone();
        move |_| {
            let Some(window) = web_sys::window() else {
                return;
            };
            let Ok(Some(value)) = window.prompt_with_message_and_default(
                "表示名（空欄で自動名に戻す、半角12文字まで）",
                &current_name,
            ) else {
                return;
            };
            match ManagementHostName::from_ascii(value.trim()) {
                Ok(name) => send(ManagementCommand::SetHostName {
                    host_id: host,
                    name,
                }),
                Err(_) => {
                    let _ = window.alert_with_message("名前は半角12文字以内で入力してください");
                }
            }
        }
    };
    let title = if name.is_empty() {
        format!("スロット {}", host.0)
    } else {
        name
    };
    view! {
        <article class:active=active class="slot">
            <div class="slot-head"><div class="device-identity"><span class="device-icon" aria-hidden="true">{if flags.known { "◫" } else { "+" }}</span><div><h3>{title}</h3><small>{format!("スロット {} · {}", host.0, if name_source == 1 { "機器から名前を取得" } else if name_source == 2 { "カスタム名" } else { "未登録" })}</small></div></div>{active.then(|| view! { <span class="badge selected">"送信先"</span> })}</div>
            <div class="badges">{status_badges(flags)}</div>
            <p class="timing">{timing.map(|value| format!("最終接続 {}秒前 · 切断理由 0x{:02x}", value.last_connected_seconds, value.last_disconnect_reason)).unwrap_or_else(|| if flags.known { "接続履歴はありません".into() } else { "ここにPCやスマートフォンを追加できます".into() })}</p>
            <div class="slot-actions">
                <button on:click=select disabled=move || busy.get() || !flags.known || active>{if active { "選択中" } else { "この機器へ切り替え" }}</button>
                <button class="secondary" on:click=pair disabled=move || busy.get() || flags.bonded || pairing>{if pairing { "ペアリング中…" } else if flags.known { "再ペアリング" } else { "新しい機器を追加" }}</button>
                <button class="quiet compact" on:click=rename disabled=move || busy.get() || !flags.known>"名前を変更"</button>
                <button class="danger compact" on:click=forget disabled=move || busy.get() || !flags.known>"登録解除"</button>
            </div>
        </article>
    }
}

fn usb_device_card(device: UsbDeviceView) -> impl IntoView {
    let name = if device.name.is_empty() {
        "名前未取得".into()
    } else {
        device.name
    };
    view! { <article class="device-card"><div class="device-identity"><span class="device-icon usb" aria-hidden="true">"⌨"</span><div><h3>{name}</h3><p>"USBで接続中"</p></div><span class="live-indicator">"● 接続中"</span></div><div class="badges">{(device.flags & 0x02 != 0).then(|| view!{<span class="badge">"キーボード"</span>})}{(device.flags & 0x04 != 0).then(|| view!{<span class="badge">"マウス"</span>})}{(device.flags & 0x08 != 0).then(|| view!{<span class="badge">"メディア操作"</span>})}</div><div class="device-meta"><span>{format!("USB機器 {}", device.index + 1)}</span><code>{format!("{:04x}:{:04x}", device.vendor_id, device.product_id)}</code><span>{format!("ID {}", device.device_id)}</span></div></article> }
}

fn diagnostics_view(value: ManagementDiagnostics) -> impl IntoView {
    view! { <div class="diagnostics-grid">
        <article><span>"再起動理由"</span><strong>{format!("0x{:02x}", value.reset_reason)}</strong></article>
        <article><span>"brownout"</span><strong>{value.brownout_count}</strong></article>
        <article><span>"BLE notify失敗"</span><strong>{value.ble_notify_failure_count}</strong></article>
        <article><span>"USBエラー"</span><strong>{value.usb_error_count}</strong></article>
        <article><span>"Flash保存"</span><strong>{value.flash_write_count}</strong></article>
        <article><span>"Flash失敗"</span><strong>{value.flash_failure_count}</strong></article>
    </div> }
}

fn status_badges(status: ManagementHostStatus) -> impl IntoView {
    let mut badges = Vec::new();
    if status.known {
        badges.push(("登録済み", "good"));
    }
    if status.connected {
        badges.push(("接続中", "live"));
    }
    if status.encrypted {
        badges.push(("暗号化済み", "good"));
    }
    if status.bonded {
        badges.push(("bond済み", "good"));
    }
    if badges.is_empty() {
        badges.push(("空き", ""));
    }
    badges
        .into_iter()
        .map(|(label, class)| view! { <span class=format!("badge {class}")>{label}</span> })
        .collect_view()
}

fn display_host(host: Option<HostId>, names: &[String; 4]) -> String {
    host.map(|host| {
        let index = host.0.saturating_sub(1) as usize;
        if index < names.len() && !names[index].is_empty() {
            names[index].clone()
        } else {
            format!("スロット {}", host.0)
        }
    })
    .unwrap_or_else(|| "なし".into())
}

fn format_duration(seconds: u32) -> String {
    format!(
        "{}日 {:02}:{:02}:{:02}",
        seconds / 86400,
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}
fn format_history(event: &ManagementHistoryEvent) -> String {
    format!(
        "#{:04} +{}s {} subject={} detail=0x{:02x} {:04x}:{:04x}",
        event.sequence,
        event.timestamp_seconds,
        history_kind(event.kind),
        event.subject,
        event.detail,
        event.vendor_id,
        event.product_id
    )
}
fn history_kind(kind: u8) -> &'static str {
    match kind {
        1 => "BLE接続",
        2 => "BLE切断",
        3 => "USB接続",
        4 => "USB切断",
        5 => "接続先変更",
        6 => "ペアリング開始",
        _ => "イベント",
    }
}
fn result_message(result: ManagementResult) -> &'static str {
    match result {
        ManagementResult::Ok => "ok",
        ManagementResult::InvalidHost => "スロット番号が不正です",
        ManagementResult::HostNotFound => "未登録のスロットです",
        ManagementResult::HostAlreadyBonded => "bond済みです",
        ManagementResult::InternalError => "firmware内部エラーです",
        ManagementResult::InvalidName => "名前が不正です",
        ManagementResult::InvalidSetting => "設定値または対象が不正です",
        ManagementResult::NotFound => "対象が見つかりません",
        ManagementResult::Unavailable => "このfirmwareでは利用できません",
    }
}
fn client_error_message(error: BrowserClientError) -> String {
    match error {
        BrowserClientError::Busy => "別の処理が完了するまで待ってください".into(),
        BrowserClientError::Disconnected => "接続が切れました".into(),
        BrowserClientError::Transport(error) => error,
        BrowserClientError::Protocol(error) => format!("応答を解釈できません: {error}"),
        BrowserClientError::Timeout => "firmwareからの応答がタイムアウトしました".into(),
    }
}
