//! Management settings schema shared by firmware, CLI, and Web UI.
//!
//! `settings_schema!` is the single source of truth. Adding an entry generates
//! the stable wire ID, descriptor table, defaults, validation, and lookup code.

use crate::ids::HostId;

pub const SETTINGS_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SettingValueKind {
    Bool = 0,
    Integer = 1,
    Choice = 2,
    HidUsage = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SettingScope {
    Global = 0,
    Host = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettingChoice {
    pub value: i32,
    pub label: &'static str,
}

const NO_CHOICES: &[SettingChoice] = &[];
const TARGET_CHOICES: &[SettingChoice] = &[
    SettingChoice {
        value: 0,
        label: "最後に使用した接続先",
    },
    SettingChoice {
        value: 1,
        label: "スロット 1",
    },
    SettingChoice {
        value: 2,
        label: "スロット 2",
    },
    SettingChoice {
        value: 3,
        label: "スロット 3",
    },
    SettingChoice {
        value: 4,
        label: "スロット 4",
    },
];
const BUTTON_ACTION_CHOICES: &[SettingChoice] = &[
    SettingChoice {
        value: 0,
        label: "何もしない",
    },
    SettingChoice {
        value: 1,
        label: "次の接続先へ切り替え",
    },
    SettingChoice {
        value: 2,
        label: "ペアリングを開始",
    },
    SettingChoice {
        value: 3,
        label: "現在の機器を登録解除",
    },
];
const KEYBOARD_LAYOUT_CHOICES: &[SettingChoice] = &[
    SettingChoice {
        value: 0,
        label: "変換しない",
    },
    SettingChoice {
        value: 1,
        label: "US配列向け",
    },
    SettingChoice {
        value: 2,
        label: "JIS配列向け",
    },
];
const LOG_LEVEL_CHOICES: &[SettingChoice] = &[
    SettingChoice {
        value: 0,
        label: "エラーのみ",
    },
    SettingChoice {
        value: 1,
        label: "警告以上",
    },
    SettingChoice {
        value: 2,
        label: "通常（推奨）",
    },
    SettingChoice {
        value: 3,
        label: "デバッグ",
    },
    SettingChoice {
        value: 4,
        label: "すべて記録",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SettingDescriptor {
    pub id: SettingId,
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: SettingValueKind,
    pub scope: SettingScope,
    pub default: i32,
    pub min: i32,
    pub max: i32,
    pub step: u16,
    pub unit: &'static str,
    pub choices: &'static [SettingChoice],
    pub restart_required: bool,
}

macro_rules! settings_schema {
    ($(
        $variant:ident = $id:literal => {
            key: $key:literal, label: $label:literal, description: $description:literal,
            kind: $kind:ident, scope: $scope:ident, default: $default:literal,
            min: $min:literal, max: $max:literal, step: $step:literal,
            unit: $unit:literal, choices: $choices:ident,
            restart: $restart:literal
        }
    ),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        #[repr(u16)]
        pub enum SettingId { $($variant = $id),+ }

        pub const SETTING_DESCRIPTORS: &[SettingDescriptor] = &[$(
            SettingDescriptor {
                id: SettingId::$variant,
                key: $key,
                label: $label,
                description: $description,
                kind: SettingValueKind::$kind,
                scope: SettingScope::$scope,
                default: $default,
                min: $min,
                max: $max,
                step: $step,
                unit: $unit,
                choices: $choices,
                restart_required: $restart,
            }
        ),+];

        impl SettingId {
            pub const fn from_u16(value: u16) -> Option<Self> {
                match value { $($id => Some(Self::$variant),)+ _ => None }
            }
        }
    };
}

settings_schema! {
    BootTarget = 1 => {
        key: "boot_target", label: "起動時の接続先", description: "0は最後の接続先、1〜4は固定スロットです",
        kind: Choice, scope: Global, default: 0, min: 0, max: 4, step: 1, unit: "", choices: TARGET_CHOICES, restart: false
    },
    RestoreLastTarget = 2 => {
        key: "restore_last_target", label: "最後の接続先を復元", description: "再起動時に最後に選択した接続先を復元します",
        kind: Bool, scope: Global, default: 1, min: 0, max: 1, step: 1, unit: "", choices: NO_CHOICES, restart: false
    },
    AutoReconnect = 3 => {
        key: "auto_reconnect", label: "自動再接続", description: "保存済み接続先からの接続を起動時から受け付けます",
        kind: Bool, scope: Global, default: 1, min: 0, max: 1, step: 1, unit: "", choices: NO_CHOICES, restart: true
    },
    SwitchReleaseDelayMs = 4 => {
        key: "switch_release_delay_ms", label: "切替時のキー解放待ち時間", description: "接続先切替前にrelease reportを処理する待ち時間です",
        kind: Integer, scope: Global, default: 20, min: 0, max: 1000, step: 5, unit: " ms", choices: NO_CHOICES, restart: false
    },
    ButtonShortAction = 5 => {
        key: "button_short_action", label: "短押し操作", description: "0:なし 1:次の接続先 2:ペアリング 3:bond削除",
        kind: Choice, scope: Global, default: 1, min: 0, max: 3, step: 1, unit: "", choices: BUTTON_ACTION_CHOICES, restart: false
    },
    ButtonLongAction = 6 => {
        key: "button_long_action", label: "長押し操作", description: "0:なし 1:次の接続先 2:ペアリング 3:bond削除",
        kind: Choice, scope: Global, default: 2, min: 0, max: 3, step: 1, unit: "", choices: BUTTON_ACTION_CHOICES, restart: false
    },
    ButtonVeryLongAction = 7 => {
        key: "button_very_long_action", label: "超長押し操作", description: "0:なし 1:次の接続先 2:ペアリング 3:bond削除",
        kind: Choice, scope: Global, default: 3, min: 0, max: 3, step: 1, unit: "", choices: BUTTON_ACTION_CHOICES, restart: false
    },
    KeyboardLayout = 8 => {
        key: "keyboard_layout", label: "キーボード配列", description: "0:そのまま 1:US(Yen→Grave) 2:JIS(Grave→Yen)",
        kind: Choice, scope: Host, default: 0, min: 0, max: 2, step: 1, unit: "", choices: KEYBOARD_LAYOUT_CHOICES, restart: false
    },
    RemapFromUsage = 9 => {
        key: "remap_from_usage", label: "変更元キー", description: "USB HID Usage ID。0でリマップ無効です",
        kind: HidUsage, scope: Host, default: 0, min: 0, max: 255, step: 1, unit: "", choices: NO_CHOICES, restart: false
    },
    RemapToUsage = 10 => {
        key: "remap_to_usage", label: "変更先キー", description: "送信するUSB HID Usage IDです",
        kind: HidUsage, scope: Host, default: 0, min: 0, max: 255, step: 1, unit: "", choices: NO_CHOICES, restart: false
    },
    MouseSensitivityPercent = 11 => {
        key: "mouse_sensitivity_percent", label: "マウス感度", description: "移動量の倍率を百分率で指定します",
        kind: Integer, scope: Host, default: 100, min: 10, max: 400, step: 5, unit: "%", choices: NO_CHOICES, restart: false
    },
    ScrollMultiplierPercent = 12 => {
        key: "scroll_multiplier_percent", label: "スクロール倍率", description: "ホイール移動量の倍率を百分率で指定します",
        kind: Integer, scope: Host, default: 100, min: 10, max: 400, step: 5, unit: "%", choices: NO_CHOICES, restart: false
    },
    ConsumerFromUsage = 13 => {
        key: "consumer_from_usage", label: "Consumer変更元", description: "Consumer Control Usage ID。0で無効です",
        kind: HidUsage, scope: Host, default: 0, min: 0, max: 4095, step: 1, unit: "", choices: NO_CHOICES, restart: false
    },
    ConsumerToUsage = 14 => {
        key: "consumer_to_usage", label: "Consumer変更先", description: "送信するConsumer Control Usage IDです",
        kind: HidUsage, scope: Host, default: 0, min: 0, max: 4095, step: 1, unit: "", choices: NO_CHOICES, restart: false
    },
    LogLevel = 15 => {
        key: "log_level", label: "ログレベル", description: "0:error 1:warn 2:info 3:debug 4:trace",
        kind: Choice, scope: Global, default: 2, min: 0, max: 4, step: 1, unit: "", choices: LOG_LEVEL_CHOICES, restart: false
    },
}

pub const SETTING_COUNT: usize = SETTING_DESCRIPTORS.len();
pub const SETTINGS_SCHEMA_HASH: u32 = {
    let mut hash = 0x811c_9dc5u32;
    hash ^= SETTINGS_SCHEMA_VERSION as u32;
    hash = hash.wrapping_mul(0x0100_0193);
    hash ^= SETTING_COUNT as u32;
    hash.wrapping_mul(0x0100_0193)
};

pub const fn setting_descriptor(id: SettingId) -> &'static SettingDescriptor {
    let mut index = 0;
    while index < SETTING_DESCRIPTORS.len() {
        if SETTING_DESCRIPTORS[index].id as u16 == id as u16 {
            return &SETTING_DESCRIPTORS[index];
        }
        index += 1;
    }
    &SETTING_DESCRIPTORS[0]
}

pub fn setting_by_key(key: &str) -> Option<&'static SettingDescriptor> {
    SETTING_DESCRIPTORS
        .iter()
        .find(|setting| setting.key == key)
}

pub const fn validate_setting_value(id: SettingId, value: i32) -> bool {
    let descriptor = setting_descriptor(id);
    value >= descriptor.min && value <= descriptor.max
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GlobalSettings {
    pub boot_target: u8,
    pub restore_last_target: bool,
    pub auto_reconnect: bool,
    pub switch_release_delay_ms: u16,
    pub button_short_action: u8,
    pub button_long_action: u8,
    pub button_very_long_action: u8,
    pub log_level: u8,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            boot_target: 0,
            restore_last_target: true,
            auto_reconnect: true,
            switch_release_delay_ms: 20,
            button_short_action: 1,
            button_long_action: 2,
            button_very_long_action: 3,
            log_level: 2,
        }
    }
}

impl GlobalSettings {
    pub const DEFAULT: Self = Self {
        boot_target: 0,
        restore_last_target: true,
        auto_reconnect: true,
        switch_release_delay_ms: 20,
        button_short_action: 1,
        button_long_action: 2,
        button_very_long_action: 3,
        log_level: 2,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostSettings {
    pub keyboard_layout: u8,
    pub remap_from_usage: u8,
    pub remap_to_usage: u8,
    pub mouse_sensitivity_percent: u16,
    pub scroll_multiplier_percent: u16,
    pub consumer_from_usage: u16,
    pub consumer_to_usage: u16,
}

impl Default for HostSettings {
    fn default() -> Self {
        Self {
            keyboard_layout: 0,
            remap_from_usage: 0,
            remap_to_usage: 0,
            mouse_sensitivity_percent: 100,
            scroll_multiplier_percent: 100,
            consumer_from_usage: 0,
            consumer_to_usage: 0,
        }
    }
}

impl HostSettings {
    pub const DEFAULT: Self = Self {
        keyboard_layout: 0,
        remap_from_usage: 0,
        remap_to_usage: 0,
        mouse_sensitivity_percent: 100,
        scroll_multiplier_percent: 100,
        consumer_from_usage: 0,
        consumer_to_usage: 0,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingTarget {
    Global,
    Host(HostId),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_schema_has_stable_unique_ids_and_keys() {
        assert_eq!(SETTING_COUNT, 15);
        for (index, left) in SETTING_DESCRIPTORS.iter().enumerate() {
            assert_eq!(SettingId::from_u16(left.id as u16), Some(left.id));
            assert!(!left.key.is_empty());
            assert!(!left.label.is_empty());
            assert!(left.default >= left.min && left.default <= left.max);
            for right in &SETTING_DESCRIPTORS[index + 1..] {
                assert_ne!(left.id, right.id);
                assert_ne!(left.key, right.key);
            }
        }
    }

    #[test]
    fn every_choice_setting_has_complete_human_readable_options() {
        for descriptor in SETTING_DESCRIPTORS {
            if descriptor.kind != SettingValueKind::Choice {
                assert!(descriptor.choices.is_empty());
                continue;
            }

            assert!(!descriptor.choices.is_empty(), "{}", descriptor.key);
            for value in descriptor.min..=descriptor.max {
                assert!(
                    descriptor
                        .choices
                        .iter()
                        .any(|choice| choice.value == value),
                    "{} has no label for {value}",
                    descriptor.key
                );
            }
            assert!(
                descriptor
                    .choices
                    .iter()
                    .all(|choice| !choice.label.is_empty())
            );
        }
    }

    #[test]
    fn generated_validation_uses_declared_ranges() {
        assert!(validate_setting_value(
            SettingId::MouseSensitivityPercent,
            400
        ));
        assert!(!validate_setting_value(
            SettingId::MouseSensitivityPercent,
            401
        ));
        assert_eq!(
            setting_by_key("auto_reconnect").unwrap().id,
            SettingId::AutoReconnect
        );
    }
}
