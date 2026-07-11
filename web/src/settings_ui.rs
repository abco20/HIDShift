use std::rc::Rc;

use hidshift::{
    HostId, ManagementCommand, SettingDescriptor, SettingId, SettingScope, SettingTarget,
    SettingValueKind,
};
use leptos::prelude::*;
use send_wrapper::SendWrapper;

use crate::app::SettingView;

type CommandSender = SendWrapper<Rc<dyn Fn(ManagementCommand)>>;

#[component]
pub(crate) fn SettingsPanel(
    settings: RwSignal<Vec<SettingView>>,
    busy: RwSignal<bool>,
    send: CommandSender,
) -> impl IntoView {
    let global_send = send.clone();
    view! {
        <div class="settings-panel">
            <section class="setting-group">
                <div class="setting-group-title"><h3>"本体の動作"</h3><p>"すべての接続先に共通する設定です。"</p></div>
                <div class="settings-list">{move || settings.get().into_iter().filter(|entry| entry.descriptor.scope == SettingScope::Global).map(|entry| view! { <SettingControl entry busy send=global_send.clone()/> }).collect_view()}</div>
            </section>
            <section class="setting-group">
                <div class="setting-group-title"><h3>"接続先ごとの入力"</h3><p>"配列、キー割り当て、ポインター感度を機器ごとに調整します。"</p></div>
                <div class="host-setting-groups">
                    {(1..=4).map(|slot| {
                        let host_send = send.clone();
                        view! {
                            <details>
                                <summary><span><strong>{format!("スロット {slot}")}</strong><small>"この接続先だけに適用"</small></span><span class="chevron">"⌄"</span></summary>
                                <div class="settings-list host-settings">{move || settings.get().into_iter().filter(move |entry| entry.target == SettingTarget::Host(HostId(slot))).map(|entry| view! { <SettingControl entry busy send=host_send.clone()/> }).collect_view()}</div>
                            </details>
                        }
                    }).collect_view()}
                </div>
            </section>
        </div>
    }
}

#[component]
fn SettingControl(entry: SettingView, busy: RwSignal<bool>, send: CommandSender) -> impl IntoView {
    let descriptor = entry.descriptor;
    let control = match descriptor.kind {
        SettingValueKind::Bool => bool_control(entry, busy, send).into_any(),
        SettingValueKind::Choice => choice_control(entry, busy, send).into_any(),
        SettingValueKind::Integer => range_control(entry, busy, send).into_any(),
        SettingValueKind::HidUsage => usage_control(entry, busy, send).into_any(),
    };
    view! {
        <article class="setting-row">
            <div class="setting-copy"><h4>{descriptor.label}{descriptor.restart_required.then(|| view! { <span class="restart-badge">"再起動後に反映"</span> })}</h4><p>{friendly_description(descriptor)}</p></div>
            <div class="setting-control">{control}</div>
        </article>
    }
}

fn bool_control(entry: SettingView, busy: RwSignal<bool>, send: CommandSender) -> impl IntoView {
    let update = move |event| send_value(&send, entry, i32::from(event_target_checked(&event)));
    view! { <label class="switch"><input type="checkbox" prop:checked=entry.value != 0 disabled=move || busy.get() on:change=update/><span></span><strong>{if entry.value != 0 { "オン" } else { "オフ" }}</strong></label> }
}

fn choice_control(entry: SettingView, busy: RwSignal<bool>, send: CommandSender) -> impl IntoView {
    let update = move |event| {
        if let Ok(value) = event_target_value(&event).parse() {
            send_value(&send, entry, value);
        }
    };
    view! { <select disabled=move || busy.get() on:change=update>{entry.descriptor.choices.iter().map(|choice| view! { <option value=choice.value selected=choice.value == entry.value>{choice.label}</option> }).collect_view()}</select> }
}

fn range_control(entry: SettingView, busy: RwSignal<bool>, send: CommandSender) -> impl IntoView {
    let display = RwSignal::new(entry.value);
    let descriptor = entry.descriptor;
    let update_display = move |event| {
        if let Ok(value) = event_target_value(&event).parse() {
            display.set(value);
        }
    };
    let reset_send = send.clone();
    let commit = move |event| {
        if let Ok(value) = event_target_value(&event).parse() {
            send_value(&send, entry, value);
        }
    };
    view! { <div class="range-control"><output>{move || format!("{}{}", display.get(), descriptor.unit)}</output><input type="range" min=descriptor.min max=descriptor.max step=descriptor.step value=entry.value disabled=move || busy.get() on:input=update_display on:change=commit/><div class="range-labels"><span>{format!("{}{}", descriptor.min, descriptor.unit)}</span><button type="button" class="text-button" disabled=move || busy.get() on:click=move |_| send_value(&reset_send, entry, descriptor.default)>"標準に戻す"</button><span>{format!("{}{}", descriptor.max, descriptor.unit)}</span></div></div> }
}

fn usage_control(entry: SettingView, busy: RwSignal<bool>, send: CommandSender) -> impl IntoView {
    let choices = usage_choices(entry.descriptor);
    let advanced_send = send.clone();
    let update = move |event| {
        if let Ok(value) = event_target_value(&event).parse() {
            send_value(&send, entry, value);
        }
    };
    let advanced = move |event| {
        if let Ok(value) = event_target_value(&event).parse::<i32>()
            && (entry.descriptor.min..=entry.descriptor.max).contains(&value)
        {
            send_value(&advanced_send, entry, value);
        }
    };
    let known = choices.iter().any(|(value, _)| *value == entry.value);
    view! { <div class="usage-control"><select disabled=move || busy.get() on:change=update>{(!known).then(|| view! { <option value=entry.value selected=true>{format!("現在の割り当て (Usage {})", entry.value)}</option> })}{choices.into_iter().map(|(value, label)| view! { <option value selected=value == entry.value>{label}</option> }).collect_view()}</select><details class="advanced-usage"><summary>"詳細なUsage IDを指定"</summary><label>"Usage ID"<input type="number" min=entry.descriptor.min max=entry.descriptor.max value=entry.value disabled=move || busy.get() on:change=advanced/></label></details></div> }
}

fn send_value(send: &CommandSender, entry: SettingView, value: i32) {
    send(ManagementCommand::SetSetting {
        id: entry.descriptor.id,
        target: entry.target,
        value,
    });
}

fn friendly_description(descriptor: &SettingDescriptor) -> &'static str {
    match descriptor.id {
        SettingId::BootTarget => "電源を入れたとき、最初に入力を送る機器を選びます。",
        SettingId::SwitchReleaseDelayMs => {
            "切り替え時のキー押しっぱなしを防ぐ待ち時間です。通常は変更不要です。"
        }
        SettingId::RemapFromUsage => {
            "別のキーへ置き換えたい元のキーです。「変更しない」で無効になります。"
        }
        SettingId::RemapToUsage => "置き換え後に接続先へ送るキーです。",
        SettingId::MouseSensitivityPercent => {
            "100%が元の速度です。小さくすると遅く、大きくすると速くなります。"
        }
        SettingId::ScrollMultiplierPercent => {
            "100%が元の量です。小さくすると細かく、大きくすると速くスクロールします。"
        }
        SettingId::ConsumerFromUsage => {
            "置き換えたい音量・再生などの操作です。「変更しない」で無効になります。"
        }
        SettingId::ConsumerToUsage => "置き換え後に送るメディア操作です。",
        _ => descriptor.description,
    }
}

fn usage_choices(descriptor: &SettingDescriptor) -> Vec<(i32, &'static str)> {
    if descriptor.max <= 255 {
        vec![
            (0, "変更しない"),
            (4, "A"),
            (5, "B"),
            (6, "C"),
            (7, "D"),
            (8, "E"),
            (9, "F"),
            (10, "G"),
            (11, "H"),
            (12, "I"),
            (13, "J"),
            (14, "K"),
            (15, "L"),
            (16, "M"),
            (17, "N"),
            (18, "O"),
            (19, "P"),
            (20, "Q"),
            (21, "R"),
            (22, "S"),
            (23, "T"),
            (24, "U"),
            (25, "V"),
            (26, "W"),
            (27, "X"),
            (28, "Y"),
            (29, "Z"),
            (30, "1"),
            (31, "2"),
            (32, "3"),
            (33, "4"),
            (34, "5"),
            (35, "6"),
            (36, "7"),
            (37, "8"),
            (38, "9"),
            (39, "0"),
            (40, "Enter"),
            (41, "Escape"),
            (42, "Backspace"),
            (43, "Tab"),
            (44, "Space"),
            (58, "F1"),
            (59, "F2"),
            (60, "F3"),
            (61, "F4"),
            (62, "F5"),
            (63, "F6"),
            (64, "F7"),
            (65, "F8"),
            (66, "F9"),
            (67, "F10"),
            (68, "F11"),
            (69, "F12"),
        ]
    } else {
        vec![
            (0, "変更しない"),
            (0x00b0, "再生"),
            (0x00b1, "一時停止"),
            (0x00b5, "次の曲"),
            (0x00b6, "前の曲"),
            (0x00b7, "停止"),
            (0x00cd, "再生 / 一時停止"),
            (0x00e2, "ミュート"),
            (0x00e9, "音量を上げる"),
            (0x00ea, "音量を下げる"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_choices_have_friendly_labels_and_unique_values() {
        for descriptor in hidshift::SETTING_DESCRIPTORS
            .iter()
            .filter(|item| item.kind == SettingValueKind::HidUsage)
        {
            let choices = usage_choices(descriptor);
            assert!(
                choices
                    .iter()
                    .any(|(value, label)| *value == 0 && *label == "変更しない")
            );
            for (index, (value, label)) in choices.iter().enumerate() {
                assert!(!label.is_empty());
                assert!((descriptor.min..=descriptor.max).contains(value));
                assert!(!choices[index + 1..].iter().any(|(other, _)| other == value));
            }
        }
    }
}
