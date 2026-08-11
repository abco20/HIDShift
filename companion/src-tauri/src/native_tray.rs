use hidshift::{HostId, ManagementOutputTargetStatus, ManagementStatus};
use hidshift_manager_ui::{
    DestinationId, DestinationRoute, LocalComputerIdentity, ready_destinations,
};
use tauri::menu::{Menu, MenuItem};

pub struct TraySnapshot<'a> {
    pub connection_label: Option<&'a str>,
    pub status: Option<ManagementStatus>,
    pub output: Option<ManagementOutputTargetStatus>,
    pub host_names: &'a [Option<String>; 4],
    pub local_ble_host: Option<HostId>,
    pub local_wired: bool,
    pub computer_name: &'a str,
}

pub fn update(app: &tauri::AppHandle, state: TraySnapshot<'_>) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let Ok(menu) = Menu::new(app) else { return };
    let status_text = state
        .connection_label
        .map(|label| format!("接続済み · {label}"))
        .unwrap_or_else(|| "HIDShiftを検索中".into());
    append_item(app, &menu, "connection-status", status_text, false);

    if let Some(status) = state.status {
        let names =
            core::array::from_fn(|index| state.host_names[index].clone().unwrap_or_default());
        for destination in ready_destinations(
            status,
            state.output,
            &names,
            Some(LocalComputerIdentity {
                ble_host: state.local_ble_host,
                wired: state.local_wired,
            }),
            state.computer_name,
        ) {
            let (id, route) = match destination.id {
                DestinationId::ThisComputer => {
                    ("target-this-computer".into(), destination.preferred_route())
                }
                DestinationId::Wired => ("target-wired".into(), Some(DestinationRoute::Wired)),
                DestinationId::Ble(host) => (
                    format!("target-ble-{}", host.0),
                    Some(DestinationRoute::Ble(host)),
                ),
            };
            let Some(route) = route else { continue };
            let route_text = match destination.active_route {
                Some(DestinationRoute::Wired) => "USB",
                Some(DestinationRoute::Ble(_)) => "Bluetooth",
                None if destination.wired => "USB",
                None => "Bluetooth",
            };
            let selected = if destination.selected { "✓ " } else { "" };
            let text = if destination.this_computer {
                format!("{selected}このPC · {} ({route_text})", destination.name)
            } else if matches!(route, DestinationRoute::Wired) {
                format!("{selected}有線USB")
            } else {
                let label = if destination.name.is_empty() {
                    "接続先"
                } else {
                    &destination.name
                };
                format!("{selected}{label} ({route_text})")
            };
            append_item(app, &menu, id, text, !destination.selected);
        }
    }
    append_item(app, &menu, "show", "HIDShiftを開く", true);
    append_item(app, &menu, "quit", "終了", true);
    let _ = tray.set_menu(Some(menu));
}

fn append_item(
    app: &tauri::AppHandle,
    menu: &Menu<tauri::Wry>,
    id: impl Into<tauri::menu::MenuId>,
    text: impl AsRef<str>,
    enabled: bool,
) {
    if let Ok(item) = MenuItem::with_id(app, id, text.as_ref(), enabled, None::<&str>) {
        let _ = menu.append(&item);
    }
}
