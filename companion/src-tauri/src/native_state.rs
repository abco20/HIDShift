use std::path::PathBuf;

use hidshift::{HostId, ManagementOutputTarget, ManagementStatus};
use hidshift_manager_ui::DestinationRoute;
use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeConnectionKind {
    #[default]
    Searching,
    Usb,
    Bluetooth,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct NativePreferences {
    pub notifications_enabled: bool,
    pub notification_prompt_seen: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompanionSnapshot {
    pub revision: u64,
    pub device_revision: u64,
    pub connection_kind: NativeConnectionKind,
    pub connection_label: String,
    pub computer_name: String,
    pub local_ble_host: Option<u8>,
    pub local_wired: bool,
    pub notifications_enabled: bool,
    pub notification_prompt_seen: bool,
    pub autostart_enabled: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LocalRoutePolicy {
    follows_this_computer: bool,
    current: Option<DestinationRoute>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteDecision {
    Keep,
    Switch {
        route: DestinationRoute,
        notify_fallback: bool,
    },
}

/// Tracks synchronization of the native wired/BLE identity into firmware.
/// Firmware makes the association durable; the Companion resends it once per
/// connection so a restored or replaced device also converges.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WiredHostLinkRegistration {
    attempted: Option<HostId>,
}

impl WiredHostLinkRegistration {
    pub fn reset(&mut self) {
        self.attempted = None;
    }

    pub fn next(
        &mut self,
        supported: bool,
        wired_present: bool,
        local_ble_host: Option<HostId>,
    ) -> Option<HostId> {
        let host = supported
            .then_some(local_ble_host)
            .flatten()
            .filter(|_| wired_present)?;
        if self.attempted == Some(host) {
            return None;
        }
        self.attempted = Some(host);
        Some(host)
    }
}

pub fn retain_registered_local_host(
    status: ManagementStatus,
    local_host: Option<HostId>,
) -> Option<HostId> {
    local_host.filter(|host| {
        host.0
            .checked_sub(1)
            .map(usize::from)
            .filter(|index| *index < status.host_count.min(4) as usize)
            .is_some_and(|index| status.hosts[index].known)
    })
}

impl LocalRoutePolicy {
    pub fn select_this_computer(&mut self) {
        self.follows_this_computer = true;
    }

    pub fn select_other(&mut self) {
        self.follows_this_computer = false;
    }

    pub fn observe_external_selection(
        &mut self,
        selected: ManagementOutputTarget,
        local_ble: Option<HostId>,
        local_wired: bool,
    ) {
        let belongs_here = local_route_for_target(selected, local_ble, local_wired).is_some();
        if !belongs_here {
            self.follows_this_computer = false;
        }
    }

    pub fn reconcile(
        &mut self,
        selected: ManagementOutputTarget,
        active: Option<ManagementOutputTarget>,
        local_ble: Option<HostId>,
        wired_ready: bool,
        ble_ready: bool,
    ) -> RouteDecision {
        self.current = active.and_then(|target| local_route_for_target(target, local_ble, true));
        if !self.follows_this_computer {
            return RouteDecision::Keep;
        }
        let selected = local_route_for_target(selected, local_ble, true);
        if selected.is_none() {
            // Automatic routing only changes transport within this computer.
            // A selection outside that identity therefore came from an
            // explicit UI command or the physical button and wins.
            self.follows_this_computer = false;
            return RouteDecision::Keep;
        }
        let desired = if wired_ready {
            Some(DestinationRoute::Wired)
        } else if ble_ready {
            local_ble.map(DestinationRoute::Ble)
        } else {
            None
        };
        let Some(desired) = desired else {
            return RouteDecision::Keep;
        };
        // Selection is authoritative while the Device S3 presentation or BLE
        // readiness is still transitioning. Looking only at `active` makes a
        // transient None enqueue the same selection again for every status
        // response, creating a self-sustaining management request loop.
        if selected == Some(desired) {
            return RouteDecision::Keep;
        }
        RouteDecision::Switch {
            route: desired,
            notify_fallback: matches!(self.current, Some(DestinationRoute::Wired))
                && matches!(desired, DestinationRoute::Ble(_)),
        }
    }
}

fn local_route_for_target(
    target: ManagementOutputTarget,
    local_ble: Option<HostId>,
    local_wired: bool,
) -> Option<DestinationRoute> {
    match target {
        ManagementOutputTarget::Wired if local_wired => Some(DestinationRoute::Wired),
        ManagementOutputTarget::Ble(host) if Some(host) == local_ble => {
            Some(DestinationRoute::Ble(host))
        }
        ManagementOutputTarget::Wired | ManagementOutputTarget::Ble(_) => None,
    }
}

pub fn os_computer_name() -> String {
    hostname::get()
        .ok()
        .map(|value| value.to_string_lossy().trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "This computer".into())
}

pub fn firmware_host_name(name: &str) -> Option<hidshift::ManagementHostName> {
    let mut normalized = String::new();
    let mut replacing = false;
    for character in name.trim().chars() {
        if character.is_ascii_graphic() || character == ' ' {
            if normalized.len() >= hidshift::MANAGEMENT_HOST_NAME_LEN {
                break;
            }
            normalized.push(character);
            replacing = false;
        } else if !replacing && normalized.len() < hidshift::MANAGEMENT_HOST_NAME_LEN {
            normalized.push('-');
            replacing = true;
        }
    }
    let normalized = normalized.trim_matches([' ', '-']);
    if normalized.is_empty() {
        None
    } else {
        hidshift::ManagementHostName::from_ascii(normalized).ok()
    }
}

pub fn standard_wired_present() -> bool {
    let Ok(api) = hidapi::HidApi::new() else {
        return false;
    };
    api.device_list().any(|device| {
        matches!(device.bus_type(), hidapi::BusType::Usb)
            && device.vendor_id() == hidshift::fallback::FALLBACK_USB_VENDOR_ID
            && device.product_id() == hidshift::fallback::FALLBACK_USB_PRODUCT_ID
    })
}

pub fn load_preferences(app: &tauri::AppHandle) -> NativePreferences {
    let Some(path) = preference_path(app) else {
        return NativePreferences::default();
    };
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_preferences(
    app: &tauri::AppHandle,
    preferences: &NativePreferences,
) -> Result<(), String> {
    let path = preference_path(app).ok_or("app config directory is unavailable")?;
    let parent = path.parent().ok_or("invalid app config path")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec(preferences).map_err(|error| error.to_string())?;
    std::fs::write(path, bytes).map_err(|error| error.to_string())
}

fn preference_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|directory| directory.join("companion-preferences.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_route_prefers_wired_and_only_notifies_on_fallback() {
        let host = HostId(2);
        let mut policy = LocalRoutePolicy::default();
        policy.select_this_computer();
        assert_eq!(
            policy.reconcile(
                ManagementOutputTarget::Ble(host),
                Some(ManagementOutputTarget::Ble(host)),
                Some(host),
                true,
                true
            ),
            RouteDecision::Switch {
                route: DestinationRoute::Wired,
                notify_fallback: false,
            }
        );
        assert_eq!(
            policy.reconcile(
                ManagementOutputTarget::Wired,
                Some(ManagementOutputTarget::Wired),
                Some(host),
                false,
                true,
            ),
            RouteDecision::Switch {
                route: DestinationRoute::Ble(host),
                notify_fallback: true,
            }
        );
    }

    #[test]
    fn pending_activation_does_not_reselect_the_already_selected_route() {
        let host = HostId(1);
        let mut policy = LocalRoutePolicy::default();
        policy.select_this_computer();

        assert_eq!(
            policy.reconcile(ManagementOutputTarget::Wired, None, Some(host), true, true,),
            RouteDecision::Keep
        );
        assert_eq!(
            policy.reconcile(
                ManagementOutputTarget::Ble(host),
                None,
                Some(host),
                false,
                true,
            ),
            RouteDecision::Keep
        );
    }

    #[test]
    fn selecting_another_computer_stops_automatic_routing() {
        let mut policy = LocalRoutePolicy::default();
        policy.select_this_computer();
        policy.observe_external_selection(
            ManagementOutputTarget::Ble(HostId(3)),
            Some(HostId(2)),
            true,
        );
        assert_eq!(
            policy.reconcile(
                ManagementOutputTarget::Ble(HostId(3)),
                None,
                Some(HostId(2)),
                true,
                true,
            ),
            RouteDecision::Keep
        );
    }

    #[test]
    fn firmware_selection_of_another_computer_overrides_follow_mode() {
        let mut policy = LocalRoutePolicy::default();
        policy.select_this_computer();

        assert_eq!(
            policy.reconcile(
                ManagementOutputTarget::Ble(HostId(3)),
                Some(ManagementOutputTarget::Ble(HostId(3))),
                Some(HostId(2)),
                true,
                true,
            ),
            RouteDecision::Keep
        );
        assert_eq!(
            policy.reconcile(
                ManagementOutputTarget::Ble(HostId(3)),
                Some(ManagementOutputTarget::Ble(HostId(3))),
                Some(HostId(2)),
                true,
                true,
            ),
            RouteDecision::Keep
        );
    }

    #[test]
    fn wired_host_link_is_registered_once_per_identity_and_connection() {
        let mut registration = WiredHostLinkRegistration::default();

        assert_eq!(
            registration.next(true, true, Some(HostId(2))),
            Some(HostId(2))
        );
        assert_eq!(registration.next(true, true, Some(HostId(2))), None);
        assert_eq!(registration.next(true, false, Some(HostId(3))), None);
        assert_eq!(registration.next(false, true, Some(HostId(3))), None);
        assert_eq!(
            registration.next(true, true, Some(HostId(3))),
            Some(HostId(3))
        );

        registration.reset();
        assert_eq!(
            registration.next(true, true, Some(HostId(3))),
            Some(HostId(3))
        );
    }

    #[test]
    fn forgotten_local_host_invalidates_the_companion_identity() {
        let host = HostId(1);
        let mut status = ManagementStatus::empty(4);
        status.hosts[0].known = true;
        assert_eq!(retain_registered_local_host(status, Some(host)), Some(host));

        status.hosts[0].known = false;
        assert_eq!(retain_registered_local_host(status, Some(host)), None);
        assert_eq!(retain_registered_local_host(status, None), None);
    }

    #[test]
    fn firmware_name_is_bounded_ascii_without_erasing_manual_intent() {
        assert_eq!(
            firmware_host_name("desktop-long-name").unwrap().as_bytes(),
            b"desktop-long"
        );
        assert_eq!(firmware_host_name("PC-日本").unwrap().as_bytes(), b"PC");
        assert!(firmware_host_name("日本").is_none());
    }
}
