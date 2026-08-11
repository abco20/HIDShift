//! Platform-neutral view-model helpers shared by Web and desktop frontends.

use hidshift::{
    HostId, KeyUsage, KeyboardShortcut, ManagementCommand, ManagementOutputTarget,
    ManagementOutputTargetStatus, ManagementStatus, ModifierState,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationId {
    ThisComputer,
    Wired,
    Ble(HostId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationRoute {
    Wired,
    Ble(HostId),
}

impl DestinationRoute {
    pub const fn target(self) -> ManagementOutputTarget {
        match self {
            Self::Wired => ManagementOutputTarget::Wired,
            Self::Ble(host) => ManagementOutputTarget::Ble(host),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LocalComputerIdentity {
    pub ble_host: Option<HostId>,
    /// True only when the native frontend has identified the standard
    /// HIDShift Wired USB device on this computer.
    pub wired: bool,
}

/// Returns whether a physical output route belongs to the computer running
/// the native frontend. Notifications and destination lists must use this
/// logical identity instead of presenting USB and Bluetooth as computers.
pub const fn target_belongs_to_local_computer(
    target: ManagementOutputTarget,
    local: Option<LocalComputerIdentity>,
) -> bool {
    let Some(local) = local else {
        return false;
    };
    match target {
        ManagementOutputTarget::Wired => local.wired,
        ManagementOutputTarget::Ble(host) => match local.ble_host {
            Some(local_host) => local_host.0 == host.0,
            None => false,
        },
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestinationView {
    pub id: DestinationId,
    pub name: String,
    pub this_computer: bool,
    pub wired: bool,
    pub ble_host: Option<HostId>,
    pub active_route: Option<DestinationRoute>,
    pub selected: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReadyRouteView {
    pub route: DestinationRoute,
    pub name: String,
    pub this_computer: bool,
    pub active: bool,
    pub selected: bool,
}

impl DestinationView {
    pub const fn preferred_route(&self) -> Option<DestinationRoute> {
        if self.wired {
            Some(DestinationRoute::Wired)
        } else if let Some(host) = self.ble_host {
            Some(DestinationRoute::Ble(host))
        } else {
            None
        }
    }
}

/// Builds the ready destination list used by Home and the native tray.
/// Native-only identity is optional so the browser keeps physical USB and BLE
/// destinations separate.
pub fn ready_destinations(
    status: ManagementStatus,
    output: Option<ManagementOutputTargetStatus>,
    names: &[String; 4],
    local: Option<LocalComputerIdentity>,
    local_name: &str,
) -> Vec<DestinationView> {
    let active = output
        .and_then(|value| value.active)
        .or_else(|| status.active_host.map(ManagementOutputTarget::Ble));
    let selected = output
        .map(|value| value.selected)
        .or_else(|| status.active_host.map(ManagementOutputTarget::Ble));
    let wired_ready = output.is_some_and(|value| value.wired_ready);
    let local_ble = local.and_then(|value| value.ble_host);
    let local_wired = local.is_some_and(|value| value.wired && wired_ready)
        && output.is_some_and(|value| {
            value.effective_presentation == hidshift::ManagementUsbPresentationKind::Fallback
        });
    let local_ble_ready = local_ble.is_some_and(|host| ble_ready(status, output, host));
    let mut destinations = Vec::new();

    if local_wired || local_ble_ready {
        let active_route = match active {
            Some(ManagementOutputTarget::Wired) if local_wired => Some(DestinationRoute::Wired),
            Some(ManagementOutputTarget::Ble(host)) if Some(host) == local_ble => {
                Some(DestinationRoute::Ble(host))
            }
            _ => None,
        };
        let is_selected = match selected {
            Some(ManagementOutputTarget::Wired) => local_wired,
            Some(ManagementOutputTarget::Ble(host)) => Some(host) == local_ble,
            None => false,
        };
        destinations.push(DestinationView {
            id: DestinationId::ThisComputer,
            name: local_name.to_owned(),
            this_computer: true,
            wired: local_wired,
            ble_host: local_ble.filter(|_| local_ble_ready),
            active_route,
            selected: is_selected,
        });
    }

    if wired_ready && !local_wired {
        destinations.push(DestinationView {
            id: DestinationId::Wired,
            name: String::new(),
            this_computer: false,
            wired: true,
            ble_host: None,
            active_route: (active == Some(ManagementOutputTarget::Wired))
                .then_some(DestinationRoute::Wired),
            selected: selected == Some(ManagementOutputTarget::Wired),
        });
    }

    for (index, name) in names
        .iter()
        .enumerate()
        .take(status.host_count.min(4) as usize)
    {
        let host = HostId((index + 1) as u8);
        if Some(host) == local_ble || !ble_ready(status, output, host) {
            continue;
        }
        destinations.push(DestinationView {
            id: DestinationId::Ble(host),
            name: name.clone(),
            this_computer: false,
            wired: false,
            ble_host: Some(host),
            active_route: (active == Some(ManagementOutputTarget::Ble(host)))
                .then_some(DestinationRoute::Ble(host)),
            selected: selected == Some(ManagementOutputTarget::Ble(host)),
        });
    }
    destinations
}

/// Expands logical destinations into the physical USB and Bluetooth routes
/// that are currently ready. Detailed management screens use this view while
/// Home and the tray keep showing one row per computer.
pub fn ready_routes(
    status: ManagementStatus,
    output: Option<ManagementOutputTargetStatus>,
    names: &[String; 4],
    local: Option<LocalComputerIdentity>,
    local_name: &str,
) -> Vec<ReadyRouteView> {
    let selected = output
        .map(|value| value.selected)
        .or_else(|| status.active_host.map(ManagementOutputTarget::Ble));
    let mut routes = Vec::new();

    for destination in ready_destinations(status, output, names, local, local_name) {
        if destination.wired {
            let route = DestinationRoute::Wired;
            routes.push(ReadyRouteView {
                route,
                name: destination.name.clone(),
                this_computer: destination.this_computer,
                active: destination.active_route == Some(route),
                selected: selected == Some(route.target()),
            });
        }
        if let Some(host) = destination.ble_host {
            let route = DestinationRoute::Ble(host);
            routes.push(ReadyRouteView {
                route,
                name: destination.name,
                this_computer: destination.this_computer,
                active: destination.active_route == Some(route),
                selected: selected == Some(route.target()),
            });
        }
    }

    routes
}

pub const fn select_destination_command(
    route: DestinationRoute,
    dual_s3: bool,
) -> ManagementCommand {
    match (dual_s3, route) {
        (false, DestinationRoute::Ble(host)) => ManagementCommand::SelectHost(host),
        (_, route) => ManagementCommand::SelectOutputTarget(route.target()),
    }
}

fn ble_ready(
    status: ManagementStatus,
    output: Option<ManagementOutputTargetStatus>,
    host: HostId,
) -> bool {
    let Some(index) = host.0.checked_sub(1).map(usize::from) else {
        return false;
    };
    if index >= status.host_count.min(4) as usize {
        return false;
    }
    output.map_or(status.hosts[index].connected, |value| {
        value.ready_ble_mask & (1 << index) != 0
    })
}

pub fn shortcut_from_code(
    code: &str,
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
) -> Option<KeyboardShortcut> {
    let key = code_to_usage(code)?;
    let mut modifiers = ModifierState::empty();
    modifiers.set(ModifierState::LEFT_CTRL, ctrl);
    modifiers.set(ModifierState::LEFT_SHIFT, shift);
    modifiers.set(ModifierState::LEFT_ALT, alt);
    modifiers.set(ModifierState::LEFT_GUI, meta);
    KeyboardShortcut::new(modifiers, KeyUsage(key))
}

pub const fn function_key_number(key: KeyUsage) -> Option<u8> {
    match key.0 {
        0x3a..=0x45 => Some(key.0 - 0x3a + 1),
        0x68..=0x73 => Some(key.0 - 0x68 + 13),
        _ => None,
    }
}

pub fn keyboard_usage_label(key: KeyUsage) -> String {
    if (0x04..=0x1d).contains(&key.0) {
        return char::from(b'A' + key.0 - 0x04).to_string();
    }
    if (0x1e..=0x26).contains(&key.0) {
        return char::from(b'1' + key.0 - 0x1e).to_string();
    }
    if let Some(number) = function_key_number(key) {
        return format!("F{number}");
    }
    match key.0 {
        0x27 => "0".into(),
        0x28 => "Enter".into(),
        0x29 => "Escape".into(),
        0x2a => "Backspace".into(),
        0x2b => "Tab".into(),
        0x2c => "Space".into(),
        0x46 => "Print Screen".into(),
        0x47 => "Scroll Lock".into(),
        0x48 => "Pause".into(),
        0x49 => "Insert".into(),
        0x4a => "Home".into(),
        0x4b => "Page Up".into(),
        0x4c => "Delete".into(),
        0x4d => "End".into(),
        0x4e => "Page Down".into(),
        0x4f => "Arrow Right".into(),
        0x50 => "Arrow Left".into(),
        0x51 => "Arrow Down".into(),
        0x52 => "Arrow Up".into(),
        0x53 => "Num Lock".into(),
        0x54 => "Numpad /".into(),
        0x55 => "Numpad *".into(),
        0x56 => "Numpad -".into(),
        0x57 => "Numpad +".into(),
        0x58 => "Numpad Enter".into(),
        0x59..=0x61 => format!("Numpad {}", key.0 - 0x59 + 1),
        0x62 => "Numpad 0".into(),
        0x63 => "Numpad .".into(),
        0x64 => "Intl Backslash".into(),
        0x65 => "Context Menu".into(),
        0x66 => "Power".into(),
        0x67 => "Numpad =".into(),
        0x75 => "Help".into(),
        0x77 => "Select".into(),
        0x78 => "Stop".into(),
        0x79 => "Again".into(),
        0x7a => "Undo".into(),
        0x7b => "Cut".into(),
        0x7c => "Copy".into(),
        0x7d => "Paste".into(),
        0x7e => "Find".into(),
        0x85 => "Numpad ,".into(),
        0x87 => "Intl Ro".into(),
        0x88 => "Kana Mode".into(),
        0x89 => "Intl Yen".into(),
        0x8a => "Convert".into(),
        0x8b => "Non Convert".into(),
        0x90..=0x98 => format!("Lang{}", key.0 - 0x90 + 1),
        0xa3 => "Props".into(),
        0xb6 => "Numpad (".into(),
        0xb7 => "Numpad )".into(),
        0xbb => "Numpad Backspace".into(),
        _ => format!("Usage 0x{:02X}", key.0),
    }
}

pub fn code_to_usage(code: &str) -> Option<u8> {
    if let [b'K', b'e', b'y', letter @ b'A'..=b'Z'] = code.as_bytes() {
        return Some(0x04 + letter - b'A');
    }
    if let [b'D', b'i', b'g', b'i', b't', digit @ b'1'..=b'9'] = code.as_bytes() {
        return Some(0x1e + digit - b'1');
    }
    if let [b'N', b'u', b'm', b'p', b'a', b'd', digit @ b'1'..=b'9'] = code.as_bytes() {
        return Some(0x59 + digit - b'1');
    }
    if let [b'L', b'a', b'n', b'g', number @ b'1'..=b'9'] = code.as_bytes() {
        return Some(0x90 + number - b'1');
    }
    if let Some(number) = code
        .strip_prefix('F')
        .and_then(|value| value.parse::<u8>().ok())
        && (1..=24).contains(&number)
    {
        return Some(if number <= 12 {
            0x3a + number - 1
        } else {
            0x68 + number - 13
        });
    }
    Some(match code {
        "Digit0" => 0x27,
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2a,
        "Tab" => 0x2b,
        "Space" => 0x2c,
        "Minus" => 0x2d,
        "Equal" => 0x2e,
        "BracketLeft" => 0x2f,
        "BracketRight" => 0x30,
        "Backslash" => 0x31,
        "Semicolon" => 0x33,
        "Quote" => 0x34,
        "Backquote" => 0x35,
        "Comma" => 0x36,
        "Period" => 0x37,
        "Slash" => 0x38,
        "CapsLock" => 0x39,
        "PrintScreen" => 0x46,
        "ScrollLock" => 0x47,
        "Pause" => 0x48,
        "Insert" => 0x49,
        "Home" => 0x4a,
        "PageUp" => 0x4b,
        "Delete" => 0x4c,
        "End" => 0x4d,
        "PageDown" => 0x4e,
        "ArrowRight" => 0x4f,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        "NumLock" => 0x53,
        "NumpadDivide" => 0x54,
        "NumpadMultiply" => 0x55,
        "NumpadSubtract" => 0x56,
        "NumpadAdd" => 0x57,
        "NumpadEnter" => 0x58,
        "Numpad0" => 0x62,
        "NumpadDecimal" => 0x63,
        "IntlBackslash" => 0x64,
        "ContextMenu" => 0x65,
        "Power" => 0x66,
        "NumpadEqual" => 0x67,
        "Help" => 0x75,
        "Select" => 0x77,
        "Stop" => 0x78,
        "Again" => 0x79,
        "Undo" => 0x7a,
        "Cut" => 0x7b,
        "Copy" => 0x7c,
        "Paste" => 0x7d,
        "Find" => 0x7e,
        "NumpadComma" => 0x85,
        "IntlRo" => 0x87,
        "KanaMode" => 0x88,
        "IntlYen" => 0x89,
        "Convert" => 0x8a,
        "NonConvert" => 0x8b,
        "Props" => 0xa3,
        "NumpadParenLeft" => 0xb6,
        "NumpadParenRight" => 0xb7,
        "NumpadBackspace" => 0xbb,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wired_and_bluetooth_routes_share_the_local_computer_identity() {
        let local = Some(LocalComputerIdentity {
            ble_host: Some(HostId(2)),
            wired: true,
        });

        assert!(target_belongs_to_local_computer(
            ManagementOutputTarget::Wired,
            local
        ));
        assert!(target_belongs_to_local_computer(
            ManagementOutputTarget::Ble(HostId(2)),
            local
        ));
        assert!(!target_belongs_to_local_computer(
            ManagementOutputTarget::Ble(HostId(3)),
            local
        ));
        assert!(!target_belongs_to_local_computer(
            ManagementOutputTarget::Wired,
            Some(LocalComputerIdentity {
                ble_host: Some(HostId(2)),
                wired: false,
            })
        ));
    }
    use hidshift::{ManagementHostStatus, ManagementUsbPresentationKind, OutputTargetAvailability};

    fn ready_output() -> ManagementOutputTargetStatus {
        ManagementOutputTargetStatus {
            selected: ManagementOutputTarget::Ble(HostId(1)),
            active: Some(ManagementOutputTarget::Ble(HostId(1))),
            availability: OutputTargetAvailability::Ready,
            wired_ready: true,
            ready_ble_mask: 0b0011,
            effective_presentation: ManagementUsbPresentationKind::Fallback,
            mirror_configured: false,
            operation_id: 1,
        }
    }

    fn connected_status() -> ManagementStatus {
        let mut status = ManagementStatus::empty(4);
        status.hosts[0] = ManagementHostStatus {
            known: true,
            connected: true,
            encrypted: true,
            bonded: true,
        };
        status.hosts[1] = status.hosts[0];
        status.active_host = Some(HostId(1));
        status
    }

    #[test]
    fn browser_codes_map_to_layout_independent_hid_usages() {
        assert_eq!(code_to_usage("KeyA"), Some(0x04));
        assert_eq!(code_to_usage("KeyZ"), Some(0x1d));
        assert_eq!(code_to_usage("Digit1"), Some(0x1e));
        assert_eq!(code_to_usage("F13"), Some(0x68));
        assert_eq!(code_to_usage("F14"), Some(0x69));
        assert_eq!(code_to_usage("F24"), Some(0x73));
        assert_eq!(code_to_usage("F25"), None);
        assert_eq!(code_to_usage("ArrowUp"), Some(0x52));
        assert_eq!(code_to_usage("Numpad0"), Some(0x62));
        assert_eq!(code_to_usage("NumpadEnter"), Some(0x58));
        assert_eq!(code_to_usage("ContextMenu"), Some(0x65));
        assert_eq!(code_to_usage("Undo"), Some(0x7a));
        assert_eq!(code_to_usage("IntlYen"), Some(0x89));
        assert_eq!(code_to_usage("Lang1"), Some(0x90));
        assert_eq!(code_to_usage("ControlLeft"), None);
        assert_eq!(code_to_usage("AudioVolumeUp"), None);
    }

    #[test]
    fn extended_function_shortcuts_are_captured_without_modifiers() {
        let shortcut = shortcut_from_code("F14", false, false, false, false).unwrap();
        assert_eq!(shortcut.key, KeyUsage(0x69));
        assert_eq!(shortcut.modifiers, ModifierState::empty());
    }

    #[test]
    fn function_key_usages_have_human_readable_numbers() {
        assert_eq!(function_key_number(KeyUsage(0x3a)), Some(1));
        assert_eq!(function_key_number(KeyUsage(0x45)), Some(12));
        assert_eq!(function_key_number(KeyUsage(0x68)), Some(13));
        assert_eq!(function_key_number(KeyUsage(0x69)), Some(14));
        assert_eq!(function_key_number(KeyUsage(0x73)), Some(24));
        assert_eq!(function_key_number(KeyUsage(0x46)), None);
    }

    #[test]
    fn standard_keyboard_usages_have_readable_labels() {
        assert_eq!(keyboard_usage_label(KeyUsage(0x04)), "A");
        assert_eq!(keyboard_usage_label(KeyUsage(0x62)), "Numpad 0");
        assert_eq!(keyboard_usage_label(KeyUsage(0x89)), "Intl Yen");
        assert_eq!(keyboard_usage_label(KeyUsage(0xa3)), "Props");
        assert_eq!(keyboard_usage_label(KeyUsage(0xdf)), "Usage 0xDF");
    }

    #[test]
    fn captured_shortcut_packs_current_modifier_state() {
        let shortcut = shortcut_from_code("KeyK", true, true, false, false).unwrap();
        assert_eq!(shortcut.key, KeyUsage(0x0e));
        assert_eq!(
            shortcut.modifiers,
            ModifierState::LEFT_CTRL | ModifierState::LEFT_SHIFT
        );
    }

    #[test]
    fn local_wired_and_ble_routes_collapse_into_one_computer() {
        let destinations = ready_destinations(
            connected_status(),
            Some(ready_output()),
            &["Desk".into(), "Laptop".into(), "".into(), "".into()],
            Some(LocalComputerIdentity {
                ble_host: Some(HostId(1)),
                wired: true,
            }),
            "kota-pc",
        );

        assert_eq!(destinations.len(), 2);
        assert_eq!(destinations[0].id, DestinationId::ThisComputer);
        assert!(destinations[0].wired);
        assert_eq!(destinations[0].ble_host, Some(HostId(1)));
        assert_eq!(
            destinations[0].preferred_route(),
            Some(DestinationRoute::Wired)
        );
        assert_eq!(destinations[1].id, DestinationId::Ble(HostId(2)));
    }

    #[test]
    fn physical_route_list_keeps_local_wired_and_bluetooth_separate() {
        let mut output = ready_output();
        output.selected = ManagementOutputTarget::Wired;
        output.active = Some(ManagementOutputTarget::Wired);
        let routes = ready_routes(
            connected_status(),
            Some(output),
            &["Desk".into(), "Laptop".into(), "".into(), "".into()],
            Some(LocalComputerIdentity {
                ble_host: Some(HostId(1)),
                wired: true,
            }),
            "kota-pc",
        );

        assert_eq!(routes.len(), 3);
        assert_eq!(routes[0].route, DestinationRoute::Wired);
        assert!(routes[0].this_computer);
        assert!(routes[0].active);
        assert!(routes[0].selected);
        assert_eq!(routes[1].route, DestinationRoute::Ble(HostId(1)));
        assert!(routes[1].this_computer);
        assert!(!routes[1].active);
        assert!(!routes[1].selected);
        assert_eq!(routes[2].route, DestinationRoute::Ble(HostId(2)));
        assert!(!routes[2].this_computer);
    }

    #[test]
    fn browser_and_unidentified_wired_output_remain_separate() {
        let destinations = ready_destinations(
            connected_status(),
            Some(ready_output()),
            &["Desk".into(), "Laptop".into(), "".into(), "".into()],
            None,
            "",
        );

        assert_eq!(destinations[0].id, DestinationId::Wired);
        assert_eq!(destinations[1].id, DestinationId::Ble(HostId(1)));
        assert_eq!(destinations[2].id, DestinationId::Ble(HostId(2)));
    }

    #[test]
    fn local_wired_identity_is_ignored_when_firmware_is_not_ready() {
        let mut output = ready_output();
        output.wired_ready = false;
        let destinations = ready_destinations(
            connected_status(),
            Some(output),
            &["Desk".into(), "Laptop".into(), "".into(), "".into()],
            Some(LocalComputerIdentity {
                ble_host: Some(HostId(1)),
                wired: true,
            }),
            "desk-os",
        );

        assert_eq!(destinations[0].id, DestinationId::ThisComputer);
        assert!(!destinations[0].wired);
        assert_eq!(
            destinations[0].preferred_route(),
            Some(DestinationRoute::Ble(HostId(1)))
        );
    }

    #[test]
    fn mirrored_wired_output_never_merges_with_this_computer() {
        let mut output = ready_output();
        output.effective_presentation = ManagementUsbPresentationKind::Mirror;
        let destinations = ready_destinations(
            connected_status(),
            Some(output),
            &["Desk".into(), "Laptop".into(), "".into(), "".into()],
            Some(LocalComputerIdentity {
                ble_host: Some(HostId(1)),
                wired: true,
            }),
            "desk-os",
        );

        assert_eq!(destinations[0].id, DestinationId::ThisComputer);
        assert!(!destinations[0].wired);
        assert_eq!(destinations[1].id, DestinationId::Wired);
    }

    #[test]
    fn destination_commands_preserve_legacy_and_dual_s3_boundaries() {
        assert_eq!(
            select_destination_command(DestinationRoute::Ble(HostId(2)), false),
            ManagementCommand::SelectHost(HostId(2))
        );
        assert_eq!(
            select_destination_command(DestinationRoute::Wired, true),
            ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Wired)
        );
    }

    #[test]
    fn single_s3_can_still_identify_this_computer() {
        let status = connected_status();
        let destinations = ready_destinations(
            status,
            None,
            &["Desk".into(), "Laptop".into(), "".into(), "".into()],
            Some(LocalComputerIdentity {
                ble_host: Some(HostId(1)),
                wired: false,
            }),
            "desk-os",
        );

        assert_eq!(destinations[0].id, DestinationId::ThisComputer);
        assert_eq!(
            destinations[0].active_route,
            Some(DestinationRoute::Ble(HostId(1)))
        );
    }
}
