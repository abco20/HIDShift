use std::rc::Rc;

use hidshift::{
    HostId, ManagementCommand, ManagementDiagnostics, ManagementHistoryEvent, ManagementHostName,
    ManagementHostStatus, ManagementOutputTarget, ManagementOutputTargetStatus, ManagementStatus,
};
use leptos::prelude::*;
use send_wrapper::SendWrapper;
use wasm_bindgen_futures::{JsFuture, spawn_local};

use crate::settings_ui::SettingsPanel;
use crate::state::{AppState, Locale, Page, Theme, UsbDeviceView};

type CommandSender = SendWrapper<Rc<dyn Fn(ManagementCommand)>>;

#[component]
pub fn App() -> impl IntoView {
    let state = AppState::new();
    state.set_theme(state.theme.get_untracked());
    let command_state = state.clone();
    let send: CommandSender = SendWrapper::new(Rc::new(move |command| {
        command_state.send(command);
    }));
    let connect_ble_state = state.clone();
    let connect_ble: SendWrapper<Rc<dyn Fn()>> =
        SendWrapper::new(Rc::new(move || connect_ble_state.connect(true)));
    let connect_serial_state = state.clone();
    let connect_serial: SendWrapper<Rc<dyn Fn()>> =
        SendWrapper::new(Rc::new(move || connect_serial_state.connect(false)));
    let notice_message = state.message;
    let notice_error = state.is_error;
    let notice_undo = state.undo;
    let notice_locale = state.locale;
    let undo_state = state.clone();
    let connect_connected = state.connected;
    let connect_busy = state.busy;
    let connect_locale = state.locale;
    let page_state = state.clone();
    let page_send = send.clone();

    view! {
        <div class="app-shell">
            <aside class="sidebar">
                <div class="brand">
                    <span class="brand-mark" aria-hidden="true">"H"</span>
                    <span><strong>"HIDShift"</strong><small>{move || text(state.locale.get(), "デバイス設定", "Device settings")}</small></span>
                </div>
                <nav class="nav-list" aria-label="Primary">
                    <NavButton state=state.clone() page=Page::Home label_ja="ホーム" label_en="Home" icon="home"/>
                    <NavButton state=state.clone() page=Page::Destinations label_ja="接続先" label_en="Destinations" icon="screen"/>
                    <NavButton state=state.clone() page=Page::Inputs label_ja="入力機器" label_en="Inputs" icon="keyboard"/>
                    <NavButton state=state.clone() page=Page::Settings label_ja="設定" label_en="Settings" icon="settings"/>
                </nav>
                <nav class="nav-support">
                    <NavButton state=state.clone() page=Page::Support label_ja="サポート" label_en="Support" icon="support"/>
                </nav>
            </aside>

            <div class="workspace">
                <header class="topbar">
                    <div class="connection-state">
                        <span class:online=move || state.connected.get() class="status-dot"></span>
                        <span>
                            <strong>{move || if state.connected.get() {
                                let label = state.connection.get();
                                if label.is_empty() { "HIDShift".into() } else { label }
                            } else {
                                text(state.locale.get(), "未接続", "Not connected").into()
                            }}</strong>
                            <small>{move || current_target(state.status.get(), state.output_status.get(), &state.names.get(), state.locale.get())}</small>
                        </span>
                    </div>
                    <div class="top-actions">
                        <button class="quiet compact" on:click={
                            let state = state.clone();
                            move |_| state.navigate(Page::Support)
                        }>{move || text(state.locale.get(), "ヘルプ", "Help")}</button>
                        <button class="secondary icon-button" aria-label="Refresh" disabled=move || !state.connected.get() || state.busy.get() on:click={
                            let state = state.clone();
                            move |_| state.refresh()
                        }><RefreshIcon/></button>
                    </div>
                </header>

                <main class="content">
                    {move || {
                        let undo_state = undo_state.clone();
                        (!notice_message.get().is_empty()).then(|| view! {
                        <div class:error=move || notice_error.get() class="notice" role="status">
                            <span>{move || notice_message.get()}</span>
                            <span>
                                {move || notice_undo.get().map(|_| view! {
                                    <button class="quiet compact" on:click={
                                        let state = undo_state.clone();
                                        move |_| state.undo()
                                    }>{move || text(notice_locale.get(), "元に戻す", "Undo")}</button>
                                })}
                                <button class="quiet compact" aria-label="Dismiss" on:click={
                                    move |_| notice_message.set(String::new())
                                }>"×"</button>
                            </span>
                        </div>
                        })
                    }}

                    {move || (!connect_connected.get()).then(|| view! {
                        <section class="connect-panel">
                            <h2>{move || text(connect_locale.get(), "HIDShiftへ接続", "Connect to HIDShift")}</h2>
                            <p>{move || text(connect_locale.get(), "普段はBluetooth、初期設定や復旧時はUSB Serialを利用できます。接続後は必要なページのデータだけを取得します。", "Use Bluetooth for everyday access or USB Serial for setup and recovery. Only the current page is loaded after connecting.")}</p>
                            <div class="connect-actions">
                                <button disabled=move || connect_busy.get() on:click={
                                    let action = connect_ble.clone();
                                    move |_| action()
                                }>{move || text(connect_locale.get(), "Bluetoothで接続", "Connect with Bluetooth")}</button>
                                <button class="secondary" disabled=move || connect_busy.get() on:click={
                                    let action = connect_serial.clone();
                                    move |_| action()
                                }>{move || text(connect_locale.get(), "USB Serialで接続", "Connect with USB Serial")}</button>
                            </div>
                        </section>
                    })}

                    {move || match page_state.page.get() {
                        Page::Home => home_page(page_state.clone(), page_send.clone()).into_any(),
                        Page::Destinations => destinations_page(page_state.clone(), page_send.clone()).into_any(),
                        Page::Inputs => inputs_page(page_state.clone(), page_send.clone()).into_any(),
                        Page::Settings => settings_page(page_state.clone(), page_send.clone()).into_any(),
                        Page::Support => support_page(page_state.clone()).into_any(),
                    }}
                </main>
            </div>

            <nav class="mobile-nav" aria-label="Primary">
                <NavButton state=state.clone() page=Page::Home label_ja="ホーム" label_en="Home" icon="home"/>
                <NavButton state=state.clone() page=Page::Destinations label_ja="接続先" label_en="Destinations" icon="screen"/>
                <NavButton state=state.clone() page=Page::Inputs label_ja="入力" label_en="Inputs" icon="keyboard"/>
                <NavButton state=state.clone() page=Page::Settings label_ja="設定" label_en="Settings" icon="settings"/>
            </nav>
        </div>
    }
}

#[component]
fn NavButton(
    state: AppState,
    page: Page,
    label_ja: &'static str,
    label_en: &'static str,
    icon: &'static str,
) -> impl IntoView {
    let nav_state = state.clone();
    view! {
        <button class:active=move || state.page.get() == page class="nav-button" on:click=move |_| nav_state.navigate(page)>
            <NavIcon name=icon/>
            <span>{move || text(state.locale.get(), label_ja, label_en)}</span>
        </button>
    }
}

#[component]
fn NavIcon(name: &'static str) -> impl IntoView {
    let path = match name {
        "home" => "M3 10.5 12 3l9 7.5M5 9.5V21h14V9.5M9 21v-7h6v7",
        "screen" => "M4 5h16v11H4zM8 20h8M12 16v4",
        "keyboard" => "M3 6h18v12H3zM6 10h.01M9 10h.01M12 10h.01M15 10h.01M18 10h.01M7 14h10",
        "settings" => {
            "M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7ZM19 12l2-1-2-3-2 .2-1.5-1.1-.3-2.1H9l-.4 2.1-1.5 1.1L5 8l-2 3 2 1-2 1 2 3 2.1-.2 1.5 1.1L9 19h6l.3-2.1 1.6-1.1 2.1.2 2-3-2-1Z"
        }
        _ => {
            "M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20ZM9.5 9a2.5 2.5 0 1 1 4.2 1.8c-1.2 1-1.7 1.4-1.7 2.7M12 17h.01"
        }
    };
    view! { <svg class="nav-icon" viewBox="0 0 24 24" aria-hidden="true"><path d=path stroke-linecap="round" stroke-linejoin="round"/></svg> }
}

#[component]
fn RefreshIcon() -> impl IntoView {
    view! { <svg class="nav-icon" viewBox="0 0 24 24" aria-hidden="true"><path d="M20 7v5h-5M4 17v-5h5M6.1 8a7 7 0 0 1 11.6-2.2L20 8M4 16l2.3 2.2A7 7 0 0 0 18 16" stroke-linecap="round" stroke-linejoin="round"/></svg> }
}

fn page_header(
    locale: Locale,
    title_ja: &'static str,
    title_en: &'static str,
    description_ja: &'static str,
    description_en: &'static str,
) -> impl IntoView {
    view! {
        <div class="page-header">
            <div><h1>{text(locale, title_ja, title_en)}</h1><p>{text(locale, description_ja, description_en)}</p></div>
        </div>
    }
}

fn home_page(state: AppState, send: CommandSender) -> impl IntoView {
    let locale = state.locale.get();
    view! {
        {page_header(locale, "ホーム", "Home", "現在の送信先と接続中の機器を確認します。", "See the current route and connected devices.")}
        {move || if state.busy.get() && state.status.get().is_none() {
            view! { <div class="row-list"><div class="skeleton"></div></div> }.into_any()
        } else {
            let status = state.status.get();
            view! {
                <section>
                    <div class="hero-route">
                        <span class="row-icon active"><RouteIcon/></span>
                        <div class="row-copy">
                            <small>{text(locale, "現在の送信先", "Current destination")}</small>
                            <strong>{current_target(status, state.output_status.get(), &state.names.get(), locale)}</strong>
                        </div>
                        <div class="row-meta">{route_state(status, state.output_status.get(), locale)}</div>
                    </div>
                </section>
                <section class="section">
                    <div class="section-heading"><div><h2>{text(locale, "接続中の接続先", "Connected destinations")}</h2></div><button class="quiet compact" on:click={
                        let state = state.clone();
                        move |_| state.navigate(Page::Destinations)
                    }>{text(locale, "すべて表示", "View all")}</button></div>
                    <div class="row-list">{connected_destination_rows(status, state.names.get(), state.busy, send.clone(), locale)}</div>
                </section>
                <section class="section">
                    <div class="section-heading"><div><h2>{text(locale, "USB入力", "USB inputs")}</h2></div><button class="quiet compact" on:click={
                        let state = state.clone();
                        move |_| state.navigate(Page::Inputs)
                    }>{text(locale, "詳細", "Details")}</button></div>
                    <div class="row-list">
                        <div class="row">
                            <span class="row-icon"><InputIcon/></span>
                            <div class="row-copy"><strong>{status.map(|value| format!("{} {}", value.usb.device_count, text(locale, "台", "devices"))).unwrap_or_else(|| "—".into())}</strong><small>{status.map(|value| format!("{} HID interfaces", value.usb.interface_count)).unwrap_or_default()}</small></div>
                            <span class=move || if status.is_some_and(|value| value.usb.device_count > 0) { "state connected" } else { "state" }>{if status.is_some_and(|value| value.usb.device_count > 0) { text(locale, "接続中", "Connected") } else { text(locale, "未接続", "None") }}</span>
                        </div>
                    </div>
                </section>
            }.into_any()
        }}
    }
}

fn destinations_page(state: AppState, send: CommandSender) -> impl IntoView {
    let locale = state.locale.get();
    let cancel_send = send.clone();
    view! {
        <div class="page-header">
            <div><h1>{text(locale, "接続先", "Destinations")}</h1><p>{text(locale, "入力を送るPCやスマートフォンを選択・登録します。切断しても登録は保持されます。", "Select and register computers or phones. Registration remains after disconnecting.")}</p></div>
            <div class="page-actions">
                <button disabled=move || state.busy.get() || state.status.get().and_then(|status| status.pairing_host).is_some() on:click={
                    let send = send.clone();
                    move |_| {
                        let next = state.status.get_untracked().and_then(first_empty_host).unwrap_or(HostId(1));
                        send(ManagementCommand::StartPairing(next));
                    }
                }>{text(locale, "接続先を追加", "Add destination")}</button>
                <button class="secondary" disabled=move || state.busy.get() || state.status.get().and_then(|status| status.pairing_host).is_none() on:click=move |_| cancel_send(ManagementCommand::CancelPairing)>{text(locale, "中止", "Cancel")}</button>
            </div>
        </div>
        <section class="section">
            <div class="section-heading"><h2>{text(locale, "接続中", "Connected")}</h2></div>
            <div class="row-list">{connected_destination_rows(state.status.get(), state.names.get(), state.busy, send.clone(), locale)}</div>
        </section>
        <section class="section">
            <div class="section-heading"><h2>{text(locale, "登録済み", "Registered")}</h2></div>
            <div class="row-list">{registered_destination_rows(state.clone(), send, locale)}</div>
        </section>
    }
}

fn connected_destination_rows(
    status: Option<ManagementStatus>,
    names: [String; 4],
    busy: RwSignal<bool>,
    send: CommandSender,
    locale: Locale,
) -> impl IntoView {
    let Some(status) = status else {
        return view! { <div class="empty-row">{text(locale, "HIDShiftへ接続してください", "Connect to HIDShift")}</div> }.into_any();
    };
    let rows = status.hosts[..status.host_count.min(4) as usize]
        .iter()
        .enumerate()
        .filter(|(_, host)| host.connected)
        .map(|(index, host)| {
            destination_row(
                index,
                *host,
                status,
                names[index].clone(),
                None,
                busy,
                send.clone(),
                locale,
            )
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        view! { <div class="empty-row">{text(locale, "接続中の接続先はありません", "No connected destinations")}</div> }.into_any()
    } else {
        rows.into_iter().collect_view().into_any()
    }
}

fn registered_destination_rows(
    state: AppState,
    send: CommandSender,
    locale: Locale,
) -> impl IntoView {
    let Some(status) = state.status.get() else {
        return view! { <div class="empty-row">{text(locale, "HIDShiftへ接続してください", "Connect to HIDShift")}</div> }.into_any();
    };
    let names = state.names.get();
    let timings = state.timings.get();
    let rows = status.hosts[..status.host_count.min(4) as usize]
        .iter()
        .enumerate()
        .filter(|(_, host)| host.known && !host.connected)
        .map(|(index, host)| {
            destination_row(
                index,
                *host,
                status,
                names[index].clone(),
                timings[index],
                state.busy,
                send.clone(),
                locale,
            )
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        view! { <div class="empty-row">{text(locale, "切断中の登録済み接続先はありません", "No disconnected registered destinations")}</div> }.into_any()
    } else {
        rows.into_iter().collect_view().into_any()
    }
}

#[allow(clippy::too_many_arguments)]
fn destination_row(
    index: usize,
    flags: ManagementHostStatus,
    status: ManagementStatus,
    name: String,
    timing: Option<hidshift::ManagementHostTiming>,
    busy: RwSignal<bool>,
    send: CommandSender,
    locale: Locale,
) -> impl IntoView {
    let host = HostId((index + 1) as u8);
    let active = status.active_host == Some(host);
    let title = if name.is_empty() {
        format!("{} {}", text(locale, "接続先", "Destination"), host.0)
    } else {
        name
    };
    let select_send = send.clone();
    let rename_send = send.clone();
    let forget_send = send.clone();
    view! {
        <div class="row has-actions">
            <span class:active=active class="row-icon"><DestinationIcon/></span>
            <div class="row-copy"><strong>{title.clone()}</strong><small>{timing.map(|value| format!("{}s · 0x{:02x}", value.last_connected_seconds, value.last_disconnect_reason)).unwrap_or_else(|| format!("ID {}", host.0))}</small></div>
            <div class="row-meta"><span class=if flags.connected { "state connected" } else { "state" }>{if flags.connected { text(locale, "接続中", "Connected") } else { text(locale, "切断中", "Disconnected") }}</span>{active.then(|| view! { <span>{text(locale, "送信先", "Selected")}</span> })}</div>
            <div class="row-actions">
                <button class="secondary compact" disabled=move || busy.get() || !flags.connected || active on:click=move |_| select_send(ManagementCommand::SelectHost(host))>{if active { text(locale, "選択中", "Selected") } else { text(locale, "選択", "Select") }}</button>
                <button class="quiet compact" disabled=move || busy.get() on:click=move |_| {
                    let Some(window) = web_sys::window() else { return; };
                    if let Ok(Some(value)) = window.prompt_with_message_and_default(text(locale, "表示名", "Display name"), &title)
                        && let Ok(name) = ManagementHostName::from_ascii(value.trim())
                    {
                        rename_send(ManagementCommand::SetHostName { host_id: host, name });
                    }
                }>{text(locale, "名前", "Rename")}</button>
                <button class="danger compact" disabled=move || busy.get() on:click=move |_| {
                    let confirmed = web_sys::window().and_then(|window| window.confirm_with_message(text(locale, "この接続先の登録を解除しますか？", "Forget this destination?")).ok()).unwrap_or(false);
                    if confirmed { forget_send(ManagementCommand::ForgetHost(host)); }
                }>{text(locale, "登録解除", "Forget")}</button>
            </div>
        </div>
    }
}

fn inputs_page(state: AppState, send: CommandSender) -> impl IntoView {
    let locale = state.locale.get();
    view! {
        {page_header(locale, "入力機器", "Inputs", "USBキーボード、マウス、メディア操作機器を管理します。設定は入力機器に紐づきます。", "Manage USB keyboards, mice, and media controls. Settings belong to each input device.")}
        <section class="section">
            <div class="section-heading"><h2>{text(locale, "接続中", "Connected")}</h2><span class="count">{format!("{}", state.usb_devices.get().len())}</span></div>
            <div class="row-list">{input_rows(state.usb_devices.get(), locale)}</div>
        </section>
        {move || (!state.mirror_candidates.get().is_empty()).then(|| view! {
            <section class="section">
                <div class="section-heading"><div><h2>{text(locale, "有線互換モード", "Wired compatibility mode")}</h2><p>{text(locale, "Standardまたは元のUSB機器として提示します。", "Present Standard HID or the original USB device.")}</p></div></div>
                <div class="row-list">
                    <div class="row has-actions">
                        <span class="row-icon"><RouteIcon/></span>
                        <div class="row-copy"><strong>"Standard"</strong><small>{text(locale, "Keyboard / Mouse / Consumer + 管理CDC", "Keyboard / Mouse / Consumer + management CDC")}</small></div>
                        <div class="row-actions"><button class="secondary compact" disabled=move || state.busy.get() on:click={
                            let send = send.clone();
                            move |_| send(ManagementCommand::ClearMirrorTarget)
                        }>{text(locale, "選択", "Select")}</button></div>
                    </div>
                    {state.mirror_candidates.get().into_iter().map(|candidate| {
                        let candidate_send = send.clone();
                        view! {
                            <div class="row has-actions">
                                <span class:active=candidate.active() class="row-icon"><InputIcon/></span>
                                <div class="row-copy"><strong>{text(locale, "元のUSB機器", "Original USB device")}</strong><small>{format!("{:04x}:{:04x} · {:08x}", candidate.vendor_id, candidate.product_id, candidate.descriptor_hash)}</small></div>
                                <div class="row-meta">{candidate.active().then(|| view! { <span class="state connected">{text(locale, "提示中", "Active")}</span> })}</div>
                                <div class="row-actions"><button class="secondary compact" disabled=move || state.busy.get() || candidate.selected() on:click=move |_| candidate_send(ManagementCommand::SetMirrorTarget(candidate.candidate))>{if candidate.selected() { text(locale, "選択中", "Selected") } else { text(locale, "選択", "Select") }}</button></div>
                            </div>
                        }
                    }).collect_view()}
                </div>
            </section>
        })}
    }
}

fn input_rows(devices: Vec<UsbDeviceView>, locale: Locale) -> impl IntoView {
    if devices.is_empty() {
        return view! { <div class="empty-row">{text(locale, "接続中のUSB入力機器はありません", "No connected USB input devices")}</div> }.into_any();
    }
    devices
        .into_iter()
        .map(|device| {
            let name = if device.name.is_empty() {
                format!("USB {}", device.index + 1)
            } else {
                device.name
            };
            let mut kinds = Vec::new();
            if device.flags & 0x02 != 0 { kinds.push(text(locale, "キーボード", "Keyboard")); }
            if device.flags & 0x04 != 0 { kinds.push(text(locale, "マウス", "Mouse")); }
            if device.flags & 0x08 != 0 { kinds.push(text(locale, "メディア操作", "Media control")); }
            view! {
                <div class="row">
                    <span class="row-icon active"><InputIcon/></span>
                    <div class="row-copy"><strong>{name}</strong><small>{format!("{} · {:04x}:{:04x} · ID {}", kinds.join(" / "), device.vendor_id, device.product_id, device.device_id)}</small></div>
                    <span class="state connected">{text(locale, "接続中", "Connected")}</span>
                </div>
            }
        })
        .collect_view()
        .into_any()
}

fn settings_page(state: AppState, send: CommandSender) -> impl IntoView {
    let locale = state.locale.get();
    view! {
        {page_header(locale, "設定", "Settings", "表示と本体の操作を調整します。入力固有の設定は入力機器ページから管理します。", "Adjust appearance and device controls. Input-specific settings belong on the Inputs page.")}
        <section class="section">
            <div class="section-heading"><h2>{text(locale, "表示", "Appearance")}</h2></div>
            <div class="setting-list">
                <div class="setting-row">
                    <div class="setting-copy"><h4>{text(locale, "言語", "Language")}</h4><p>{text(locale, "ブラウザの言語を初期値として使用します。", "Browser language is used initially.")}</p></div>
                    <div class="setting-control"><select on:change={
                        let state = state.clone();
                        move |event| state.set_locale(if event_target_value(&event) == "en" { Locale::En } else { Locale::Ja })
                    }><option value="ja" selected=locale == Locale::Ja>"日本語"</option><option value="en" selected=locale == Locale::En>"English"</option></select></div>
                </div>
                <div class="setting-row">
                    <div class="setting-copy"><h4>{text(locale, "外観", "Theme")}</h4><p>{text(locale, "OS設定に追従するか、明るさを固定します。", "Follow the operating system or choose a fixed appearance.")}</p></div>
                    <div class="setting-control"><select on:change={
                        let state = state.clone();
                        move |event| state.set_theme(match event_target_value(&event).as_str() { "light" => Theme::Light, "dark" => Theme::Dark, _ => Theme::System })
                    }><option value="system" selected=state.theme.get() == Theme::System>{text(locale, "システム", "System")}</option><option value="light" selected=state.theme.get() == Theme::Light>{text(locale, "ライト", "Light")}</option><option value="dark" selected=state.theme.get() == Theme::Dark>{text(locale, "ダーク", "Dark")}</option></select></div>
                </div>
            </div>
        </section>
        <section class="section">
            <div class="section-heading"><div><h2>{text(locale, "詳細設定", "Advanced device settings")}</h2><p>{text(locale, "現在のfirmwareが提供する設定です。", "Settings exposed by the current firmware.")}</p></div></div>
            {move || if state.settings.get().is_empty() {
                view! { <div class="row-list"><div class="empty-row">{if state.connected.get() { text(locale, "読み込み中…", "Loading…") } else { text(locale, "HIDShiftへ接続してください", "Connect to HIDShift") }}</div></div> }.into_any()
            } else {
                view! { <SettingsPanel settings=state.settings busy=state.busy send=send.clone()/> }.into_any()
            }}
        </section>
    }
}

fn support_page(state: AppState) -> impl IntoView {
    let locale = state.locale.get();
    let copy_history = {
        let state = state.clone();
        move |_| {
            let text = state
                .history
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
        }
    };
    view! {
        {page_header(locale, "サポート", "Support", "診断、イベント履歴、firmware情報をまとめて確認します。", "Review diagnostics, event history, and firmware details.")}
        <section class="section">
            <div class="section-heading"><h2>{text(locale, "システム状態", "System status")}</h2></div>
            {move || state.diagnostics.get().map(diagnostics_view)}
        </section>
        <section class="section">
            <div class="section-heading"><div><h2>{text(locale, "イベント履歴", "Event history")}</h2><p>{text(locale, "再起動で消去されるRAM内の履歴です。", "RAM-only history, cleared on reboot.")}</p></div><button class="secondary compact" on:click=copy_history>{text(locale, "コピー", "Copy")}</button></div>
            <pre class="logs">{move || state.history.get().iter().map(format_history).collect::<Vec<_>>().join("\n")}</pre>
        </section>
        <section class="section">
            <div class="section-heading"><h2>{text(locale, "バージョン", "Version")}</h2></div>
            <div class="row-list"><div class="row"><span class="row-icon"><SupportIcon/></span><div class="row-copy"><strong>"HIDShift firmware"</strong><small>{move || state.schema.get().map(|schema| format!("{}.{}.{} · protocol v1 · schema {}", schema.firmware_major, schema.firmware_minor, schema.firmware_patch, schema.version)).unwrap_or_else(|| "—".into())}</small></div></div></div>
        </section>
        <section class="section danger-zone">
            <h2>{text(locale, "復旧操作", "Recovery actions")}</h2>
            <p>{text(locale, "再起動と工場出荷状態への初期化は、新しいmanagement protocolで提供予定です。GPIO0を押しながら起動するとStandard modeで復旧できます。", "Reboot and factory reset are provided by the new management protocol. Hold GPIO0 while booting to recover in Standard mode.")}</p>
            <div class="support-actions"><button class="danger" disabled=true>{text(locale, "工場出荷状態に戻す", "Factory reset")}</button></div>
        </section>
    }
}

fn diagnostics_view(value: ManagementDiagnostics) -> impl IntoView {
    view! {
        <div class="diagnostics-grid">
            <article><span>"Uptime"</span><strong>{format_duration(value.uptime_seconds)}</strong></article>
            <article><span>"Reset reason"</span><strong>{format!("0x{:02x}", value.reset_reason)}</strong></article>
            <article><span>"Brownout"</span><strong>{value.brownout_count}</strong></article>
            <article><span>"BLE disconnect"</span><strong>{value.ble_disconnect_count}</strong></article>
            <article><span>"Notify failure"</span><strong>{value.ble_notify_failure_count}</strong></article>
            <article><span>"USB / Flash error"</span><strong>{format!("{} / {}", value.usb_error_count, value.flash_failure_count)}</strong></article>
        </div>
    }
}

fn first_empty_host(status: ManagementStatus) -> Option<HostId> {
    status.hosts[..status.host_count.min(4) as usize]
        .iter()
        .position(|host| !host.known)
        .map(|index| HostId((index + 1) as u8))
}

fn current_target(
    status: Option<ManagementStatus>,
    output: Option<ManagementOutputTargetStatus>,
    names: &[String; 4],
    locale: Locale,
) -> String {
    match output.and_then(|output| output.active) {
        Some(ManagementOutputTarget::Wired) => text(locale, "有線USB", "Wired USB").into(),
        Some(ManagementOutputTarget::Ble(host)) => host_name(host, names, locale),
        None if output.is_some() => text(locale, "準備待ち", "Waiting").into(),
        None => status
            .and_then(|status| status.active_host)
            .map(|host| host_name(host, names, locale))
            .unwrap_or_else(|| text(locale, "送信先なし", "No destination").into()),
    }
}

fn route_state(
    status: Option<ManagementStatus>,
    output: Option<ManagementOutputTargetStatus>,
    locale: Locale,
) -> &'static str {
    if output.is_some_and(|output| output.active.is_some())
        || output.is_none() && status.and_then(|status| status.active_host).is_some()
    {
        text(locale, "送信可能", "Ready")
    } else {
        text(locale, "接続待ち", "Waiting")
    }
}

fn host_name(host: HostId, names: &[String; 4], locale: Locale) -> String {
    let index = host.0.saturating_sub(1) as usize;
    names
        .get(index)
        .filter(|name| !name.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("{} {}", text(locale, "接続先", "Destination"), host.0))
}

fn text(locale: Locale, ja: &'static str, en: &'static str) -> &'static str {
    match locale {
        Locale::Ja => ja,
        Locale::En => en,
    }
}

fn format_duration(seconds: u32) -> String {
    format!(
        "{}d {:02}:{:02}:{:02}",
        seconds / 86_400,
        seconds / 3_600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

fn format_history(event: &ManagementHistoryEvent) -> String {
    format!(
        "#{:04} +{}s kind={} subject={} detail=0x{:02x} {:04x}:{:04x}",
        event.sequence,
        event.timestamp_seconds,
        event.kind,
        event.subject,
        event.detail,
        event.vendor_id,
        event.product_id
    )
}

#[component]
fn RouteIcon() -> impl IntoView {
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 5h8a4 4 0 0 1 4 4v10M13 15l4 4 4-4M3 5h2" stroke-linecap="round" stroke-linejoin="round"/></svg> }
}

#[component]
fn DestinationIcon() -> impl IntoView {
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 5h16v11H4zM8 20h8M12 16v4" stroke-linecap="round" stroke-linejoin="round"/></svg> }
}

#[component]
fn InputIcon() -> impl IntoView {
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 6h18v12H3zM6 10h.01M9 10h.01M12 10h.01M15 10h.01M18 10h.01M7 14h10" stroke-linecap="round" stroke-linejoin="round"/></svg> }
}

#[component]
fn SupportIcon() -> impl IntoView {
    view! { <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20ZM9.5 9a2.5 2.5 0 1 1 4.2 1.8c-1.2 1-1.7 1.4-1.7 2.7M12 17h.01" stroke-linecap="round" stroke-linejoin="round"/></svg> }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_and_core_labels_are_available_in_both_languages() {
        for (ja, en) in [
            ("ホーム", "Home"),
            ("接続先", "Destinations"),
            ("入力機器", "Inputs"),
            ("設定", "Settings"),
            ("サポート", "Support"),
        ] {
            assert_eq!(text(Locale::Ja, ja, en), ja);
            assert_eq!(text(Locale::En, ja, en), en);
        }
    }

    #[test]
    fn empty_slot_selection_uses_first_available_slot() {
        let mut status = ManagementStatus::empty(4);
        status.hosts[0].known = true;
        status.hosts[1].known = true;
        assert_eq!(first_empty_host(status), Some(HostId(3)));
    }
}
