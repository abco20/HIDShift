//! Management command execution and protocol response projection.

use crate::bridge::BridgeEvent;
use crate::ids::HostId;
use crate::management::{
    ManagementCommand, ManagementDestination, ManagementHostInfo, ManagementHostName,
    ManagementHostStatus, ManagementHostTiming, ManagementRequest, ManagementResponse,
    ManagementResponsePayload, ManagementResult, ManagementSchema, ManagementSetting,
    ManagementStatus, ManagementUsbDevice, ManagementUsbStatus,
};
#[cfg(feature = "dual-s3-wired")]
use crate::management::{
    ManagementMirrorCandidate, ManagementOutputTarget, ManagementOutputTargetStatus,
    ManagementUsbPresentationKind,
};
#[cfg(feature = "dual-s3-wired")]
use crate::output_target::{MirrorCandidateId, OutputTarget, StoredMirrorTarget};
use crate::settings::{
    SETTING_COUNT, SETTINGS_SCHEMA_HASH, SETTINGS_SCHEMA_VERSION, SettingId, SettingScope,
    SettingTarget, setting_descriptor, validate_setting_value,
};
use crate::storage::{FixedName, StoragePersistPriority};

use super::{
    BridgeRuntime, PAIRING_MODE_TIMEOUT_MS, PairingModeState, RUNTIME_HISTORY_CAPACITY,
    RuntimeCommand, RuntimeEffect, RuntimeError, push_command,
};

impl<const HOSTS: usize, const USB_INTERFACES: usize> BridgeRuntime<HOSTS, USB_INTERFACES> {
    pub(super) fn handle_management_request<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        destination: ManagementDestination,
        request: ManagementRequest,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<(), RuntimeError> {
        commands.clear();
        let result = self.execute_management_command::<COMMANDS, ACTIONS>(
            request.command,
            now_ms,
            commands,
        )?;
        let payload = self.management_response_payload(destination, request.command, result);
        push_command(
            commands,
            RuntimeCommand::ManagementResponse {
                destination,
                response: ManagementResponse {
                    request_id: request.request_id,
                    result,
                    payload,
                },
            },
        )
    }

    fn execute_management_command<const COMMANDS: usize, const ACTIONS: usize>(
        &mut self,
        command: ManagementCommand,
        now_ms: u64,
        commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
    ) -> Result<ManagementResult, RuntimeError> {
        let result = if management_command_requires_storage(&command)
            && !self.storage_health.allows_persistent_mutation()
        {
            ManagementResult::Unavailable
        } else {
            match command {
                ManagementCommand::GetStatus => ManagementResult::Ok,
                ManagementCommand::SelectHost(host_id) => {
                    if !valid_management_host::<HOSTS>(host_id) {
                        ManagementResult::InvalidHost
                    } else if self.bridge.state().hosts.host(host_id).is_none() {
                        ManagementResult::HostNotFound
                    } else {
                        self.request_target_switch_append::<COMMANDS, ACTIONS>(
                            host_id, now_ms, commands,
                        )?;
                        ManagementResult::Ok
                    }
                }
                ManagementCommand::StartPairing(host_id) => {
                    if !valid_management_host::<HOSTS>(host_id) {
                        ManagementResult::InvalidHost
                    } else if self
                        .bridge
                        .state()
                        .hosts
                        .host(host_id)
                        .is_some_and(|host| host.bonded || host.bond.is_some())
                    {
                        ManagementResult::HostAlreadyBonded
                    } else {
                        self.pairing_mode = Some(PairingModeState {
                            host_id,
                            deadline_ms: now_ms.saturating_add(PAIRING_MODE_TIMEOUT_MS),
                        });
                        self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                            BridgeEvent::EnterPairingMode { host_id },
                            commands,
                        )?;
                        ManagementResult::Ok
                    }
                }
                ManagementCommand::ForgetHost(host_id) => {
                    if !valid_management_host::<HOSTS>(host_id) {
                        ManagementResult::InvalidHost
                    } else if self.bridge.state().hosts.host(host_id).is_none() {
                        ManagementResult::HostNotFound
                    } else {
                        self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                            BridgeEvent::ClearHost { host_id },
                            commands,
                        )?;
                        ManagementResult::Ok
                    }
                }
                ManagementCommand::GetHostInfo(host_id) => {
                    if !valid_management_host::<HOSTS>(host_id) {
                        ManagementResult::InvalidHost
                    } else if self.bridge.state().hosts.host(host_id).is_none() {
                        ManagementResult::HostNotFound
                    } else {
                        ManagementResult::Ok
                    }
                }
                ManagementCommand::SetHostName { host_id, name } => {
                    if !valid_management_host::<HOSTS>(host_id) {
                        ManagementResult::InvalidHost
                    } else if self.bridge.state().hosts.host(host_id).is_none() {
                        ManagementResult::HostNotFound
                    } else {
                        let name = core::str::from_utf8(name.as_bytes())
                            .ok()
                            .and_then(FixedName::from_ascii);
                        if let Some(name) = name {
                            self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                                BridgeEvent::SetHostName { host_id, name },
                                commands,
                            )?;
                            ManagementResult::Ok
                        } else {
                            ManagementResult::InvalidName
                        }
                    }
                }
                ManagementCommand::CancelPairing => {
                    if let Some(pairing) = self.pairing_mode.take() {
                        self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                            BridgeEvent::PairingModeExpired {
                                host_id: pairing.host_id,
                            },
                            commands,
                        )?;
                        ManagementResult::Ok
                    } else {
                        ManagementResult::NotFound
                    }
                }
                ManagementCommand::GetUsbDevice { index, .. } => {
                    if self.management_usb_device(index, 0).is_some() {
                        ManagementResult::Ok
                    } else {
                        ManagementResult::NotFound
                    }
                }
                ManagementCommand::GetDiagnostics
                | ManagementCommand::GetHistory { .. }
                | ManagementCommand::GetSchema
                | ManagementCommand::GetClientSession => ManagementResult::Ok,
                ManagementCommand::GetHostTiming(host_id) => {
                    if valid_management_host::<HOSTS>(host_id) {
                        ManagementResult::Ok
                    } else {
                        ManagementResult::InvalidHost
                    }
                }
                ManagementCommand::GetSetting { id, target } => {
                    if self.setting_value(id, target).is_some() {
                        ManagementResult::Ok
                    } else {
                        ManagementResult::InvalidSetting
                    }
                }
                ManagementCommand::SetSetting { id, target, value } => {
                    let changed = self.setting_value(id, target) != Some(value);
                    if self.set_setting(id, target, value) {
                        if changed && let SettingTarget::Input(profile_id) = target {
                            self.reset_profile_inputs::<COMMANDS, ACTIONS>(profile_id, commands)?;
                        }
                        self.push_storage_snapshot(commands, StoragePersistPriority::Critical)?;
                        if changed && id == SettingId::LogLevel {
                            push_command(
                                commands,
                                RuntimeCommand::ApplyEffect(RuntimeEffect::SetLogLevel(
                                    value as u8,
                                )),
                            )?;
                        }
                        ManagementResult::Ok
                    } else {
                        ManagementResult::InvalidSetting
                    }
                }
                #[cfg(feature = "dual-s3-wired")]
                ManagementCommand::SelectOutputTarget(target) => {
                    let output_target = target.to_output_target();
                    if output_target.validate().is_err()
                        || matches!(output_target, OutputTarget::Ble(host) if !valid_management_host::<HOSTS>(host))
                    {
                        ManagementResult::InvalidHost
                    } else {
                        self.pending_target_switch = None;
                        self.handle_bridge_event_append::<COMMANDS, ACTIONS>(
                            BridgeEvent::SelectOutputTarget {
                                target: output_target,
                            },
                            commands,
                        )?;
                        ManagementResult::Ok
                    }
                }
                #[cfg(feature = "dual-s3-wired")]
                ManagementCommand::GetOutputTargetStatus => ManagementResult::Ok,
                #[cfg(feature = "dual-s3-wired")]
                ManagementCommand::GetMirrorCandidate(candidate) => {
                    if self.mirror_candidates.get(candidate).is_some() {
                        ManagementResult::Ok
                    } else {
                        ManagementResult::NotFound
                    }
                }
                #[cfg(feature = "dual-s3-wired")]
                ManagementCommand::SetMirrorTarget(candidate) => {
                    if let (Some(profile_hash), Some(stable_id)) = (
                        self.mirror_profile_hash(candidate),
                        self.mirror_stable_id(candidate),
                    ) {
                        self.bridge
                            .set_mirror_target(Some(StoredMirrorTarget(stable_id)));
                        self.push_storage_snapshot(commands, StoragePersistPriority::Critical)?;
                        if self.bridge.state().output_target.selected == OutputTarget::Wired {
                            debug_assert_eq!(
                                self.mirror_profile_hash(candidate),
                                Some(profile_hash)
                            );
                            self.begin_presentation_transition(
                                self.effective_mirror_candidate(),
                                commands,
                            )?;
                        }
                        ManagementResult::Ok
                    } else {
                        ManagementResult::NotFound
                    }
                }
                #[cfg(feature = "dual-s3-wired")]
                ManagementCommand::ClearMirrorTarget => {
                    self.bridge.set_mirror_target(None);
                    self.push_storage_snapshot(commands, StoragePersistPriority::Critical)?;
                    if self.bridge.state().output_target.selected == OutputTarget::Wired {
                        self.begin_presentation_transition(None, commands)?;
                    }
                    ManagementResult::Ok
                }
                #[cfg(feature = "dual-s3-wired")]
                ManagementCommand::SetWiredHostLink { ble_host } => {
                    if ble_host.is_some_and(|host| {
                        !valid_management_host::<HOSTS>(host)
                            || self.bridge.state().hosts.host(host).is_none()
                    }) {
                        ManagementResult::HostNotFound
                    } else {
                        if self.bridge.state().wired_ble_host != ble_host {
                            self.bridge.set_wired_host_link(ble_host);
                            self.push_storage_snapshot(commands, StoragePersistPriority::Critical)?;
                        }
                        ManagementResult::Ok
                    }
                }
            }
        };
        Ok(result)
    }

    fn management_response_payload(
        &self,
        destination: ManagementDestination,
        command: ManagementCommand,
        result: ManagementResult,
    ) -> ManagementResponsePayload {
        match command {
            ManagementCommand::GetHostInfo(host_id)
            | ManagementCommand::SetHostName { host_id, .. }
                if result == ManagementResult::Ok =>
            {
                self.management_host_info(host_id)
                    .map(ManagementResponsePayload::HostInfo)
                    .unwrap_or(ManagementResponsePayload::None)
            }
            ManagementCommand::GetUsbDevice { index, name_offset }
                if result == ManagementResult::Ok =>
            {
                self.management_usb_device(index, name_offset)
                    .map(ManagementResponsePayload::UsbDevice)
                    .unwrap_or(ManagementResponsePayload::None)
            }
            ManagementCommand::GetDiagnostics => {
                ManagementResponsePayload::Diagnostics(self.diagnostics)
            }
            ManagementCommand::GetHostTiming(host_id) if result == ManagementResult::Ok => {
                let index = host_id.0.saturating_sub(1) as usize;
                ManagementResponsePayload::HostTiming(ManagementHostTiming {
                    host_id,
                    last_connected_seconds: self.host_last_connected_seconds[index],
                    last_disconnected_seconds: self.host_last_disconnected_seconds[index],
                    last_disconnect_reason: self.host_last_disconnect_reason[index],
                })
            }
            ManagementCommand::GetHistory { index } => self
                .history
                .iter()
                .rev()
                .nth(index as usize)
                .copied()
                .map(ManagementResponsePayload::History)
                .unwrap_or(ManagementResponsePayload::None),
            ManagementCommand::GetSchema => ManagementResponsePayload::Schema(ManagementSchema {
                version: SETTINGS_SCHEMA_VERSION,
                setting_count: SETTING_COUNT as u8,
                history_capacity: RUNTIME_HISTORY_CAPACITY as u8,
                usb_capacity: USB_INTERFACES.min(u8::MAX as usize) as u8,
                hash: SETTINGS_SCHEMA_HASH,
                firmware_major: crate::FIRMWARE_VERSION_MAJOR,
                firmware_minor: crate::FIRMWARE_VERSION_MINOR,
                firmware_patch: crate::FIRMWARE_VERSION_PATCH,
                capabilities: crate::management::MANAGEMENT_CAPABILITY_COMPANION_EVENTS
                    | if cfg!(feature = "dual-s3-wired") {
                        crate::management::MANAGEMENT_CAPABILITY_DUAL_S3_WIRED
                            | crate::management::MANAGEMENT_CAPABILITY_COMPUTER_TARGET_LINKS
                    } else {
                        0
                    },
            }),
            ManagementCommand::GetClientSession => ManagementResponsePayload::ClientSession(
                crate::management::ManagementClientSession {
                    host_id: match destination {
                        ManagementDestination::Ble(host_id) => Some(host_id),
                        ManagementDestination::WiredHid | ManagementDestination::DebugSerial => {
                            None
                        }
                    },
                },
            ),
            ManagementCommand::GetSetting { id, target }
            | ManagementCommand::SetSetting { id, target, .. }
                if result == ManagementResult::Ok =>
            {
                ManagementResponsePayload::Setting(ManagementSetting {
                    id,
                    target,
                    value: self.setting_value(id, target).unwrap_or_default(),
                })
            }
            #[cfg(feature = "dual-s3-wired")]
            ManagementCommand::SelectOutputTarget(_)
            | ManagementCommand::GetOutputTargetStatus
            | ManagementCommand::SetMirrorTarget(_)
            | ManagementCommand::ClearMirrorTarget
            | ManagementCommand::SetWiredHostLink { .. } => {
                ManagementResponsePayload::OutputTargetStatus(
                    self.management_output_target_status(),
                )
            }
            #[cfg(feature = "dual-s3-wired")]
            ManagementCommand::GetMirrorCandidate(candidate) if result == ManagementResult::Ok => {
                self.management_mirror_candidate(candidate)
                    .map(ManagementResponsePayload::MirrorCandidate)
                    .unwrap_or(ManagementResponsePayload::None)
            }
            _ => ManagementResponsePayload::Status(self.management_status()),
        }
    }

    pub fn management_status(&self) -> ManagementStatus {
        let mut status = ManagementStatus::empty(u8::try_from(HOSTS.min(4)).unwrap_or(4));
        status.active_host = self.bridge.state().hosts.active_target();
        status.pairing_host = self.bridge.state().pairable_host;
        status.usb = self.management_usb_status();
        status.storage_health = self.storage_health;
        for index in 0..HOSTS.min(4) {
            let host_id = HostId((index + 1) as u8);
            if let Some(host) = self.bridge.state().hosts.host(host_id) {
                status.hosts[index] = ManagementHostStatus {
                    known: true,
                    connected: host.connected,
                    encrypted: host.encrypted,
                    bonded: host.bonded || host.bond.is_some(),
                };
            }
        }
        status
    }

    #[cfg(feature = "dual-s3-wired")]
    fn management_mirror_candidate(
        &self,
        candidate: MirrorCandidateId,
    ) -> Option<ManagementMirrorCandidate> {
        let metadata = self.mirror_candidates.get(candidate)?;
        let selected = self.selected_mirror_candidate() == Some(candidate);
        let active = self.active_mirror_target == Some(candidate)
            && !self.presentation_transition_pending
            && self.bridge.state().output_target.active == Some(OutputTarget::Wired);
        Some(ManagementMirrorCandidate {
            candidate,
            flags: 1
                | (u8::from(selected) << 1)
                | (u8::from(active) << 2)
                | (u8::from(metadata.synthetic) << 3),
            source_device: metadata.source_device.map(|device| device.0),
            vendor_id: metadata.stable_id.vendor_id,
            product_id: metadata.stable_id.product_id,
            profile_hash: metadata.profile_hash,
            descriptor_hash: metadata.stable_id.descriptor_hash,
        })
    }

    #[cfg(feature = "dual-s3-wired")]
    pub fn management_output_target_status(&self) -> ManagementOutputTargetStatus {
        let target = self.bridge.state().output_target;
        let mut ready_ble_mask = 0u8;
        for index in 0..HOSTS.min(4) {
            let host_id = HostId((index + 1) as u8);
            if self.bridge.ble_target_ready(host_id) {
                ready_ble_mask |= 1 << index;
            }
        }
        ManagementOutputTargetStatus {
            selected: ManagementOutputTarget::from(target.selected),
            active: target.active.map(ManagementOutputTarget::from),
            availability: target.availability,
            wired_ready: self.bridge.state().wired_availability
                == crate::output_target::OutputTargetAvailability::Ready,
            ready_ble_mask,
            effective_presentation: if target.active == Some(OutputTarget::Wired)
                && !self.presentation_transition_pending
                && self.active_mirror_target.is_some()
            {
                ManagementUsbPresentationKind::Mirror
            } else {
                ManagementUsbPresentationKind::Fallback
            },
            mirror_configured: self.bridge.state().mirror_target.is_some(),
            operation_id: target.transition_operation_id,
        }
    }

    fn management_host_info(&self, host_id: HostId) -> Option<ManagementHostInfo> {
        let host = self.bridge.state().hosts.host(host_id)?;
        let (selected_name, name_source) = if host.name.as_bytes().is_empty() {
            (host.discovered_name, 1)
        } else {
            (host.name, 2)
        };
        let name = core::str::from_utf8(selected_name.as_bytes())
            .ok()
            .and_then(|name| ManagementHostName::from_ascii(name).ok())
            .unwrap_or_else(ManagementHostName::empty);
        Some(ManagementHostInfo {
            host_id,
            status: ManagementHostStatus {
                known: true,
                connected: host.connected,
                encrypted: host.encrypted,
                bonded: host.bonded || host.bond.is_some(),
            },
            name,
            name_source,
        })
    }

    fn management_usb_status(&self) -> ManagementUsbStatus {
        let mut devices = [None; USB_INTERFACES];
        let mut device_count = 0usize;
        let mut interface_count = 0usize;
        let mut keyboard_devices = [None; USB_INTERFACES];
        let mut keyboard_count = 0usize;
        for interface in self.usb_interfaces.iter().flatten() {
            interface_count += 1;
            if (interface.flags & 0x02 != 0 || interface.led_output.is_some())
                && !keyboard_devices[..keyboard_count].contains(&Some(interface.device_id))
            {
                keyboard_devices[keyboard_count] = Some(interface.device_id);
                keyboard_count += 1;
            }
            if !devices[..device_count].contains(&Some(interface.device_id)) {
                devices[device_count] = Some(interface.device_id);
                device_count += 1;
            }
        }
        ManagementUsbStatus {
            device_count: device_count.min(u8::MAX as usize) as u8,
            interface_count: interface_count.min(u8::MAX as usize) as u8,
            keyboard_count: keyboard_count.min(u8::MAX as usize) as u8,
        }
    }

    fn management_usb_device(
        &self,
        requested_index: u8,
        name_offset: u8,
    ) -> Option<ManagementUsbDevice> {
        let mut seen = [None; USB_INTERFACES];
        let mut count = 0usize;
        let mut selected = None;
        for interface in self.usb_interfaces.iter().flatten() {
            if seen[..count].contains(&Some(interface.device_id)) {
                continue;
            }
            seen[count] = Some(interface.device_id);
            if count == requested_index as usize {
                selected = Some(*interface);
                break;
            }
            count += 1;
        }
        let selected = selected?;
        let name = selected.name.as_bytes();
        let offset = (name_offset as usize).min(name.len());
        let chunk_len = (name.len() - offset).min(4);
        let mut name_chunk = [0; 4];
        name_chunk[..chunk_len].copy_from_slice(&name[offset..offset + chunk_len]);
        let mut flags = selected.flags | 0x01;
        for interface in self.usb_interfaces.iter().flatten() {
            if interface.device_id == selected.device_id {
                flags |= interface.flags;
            }
        }
        Some(ManagementUsbDevice {
            index: requested_index,
            device_id: selected.device_id.0,
            flags,
            vendor_id: selected.vendor_id,
            product_id: selected.product_id,
            input_profile_id: selected.input_profile_id?,
            name_len: name.len().min(u8::MAX as usize) as u8,
            name_offset,
            name_chunk_len: chunk_len as u8,
            name_chunk,
        })
    }

    pub(super) fn setting_value(&self, id: SettingId, target: SettingTarget) -> Option<i32> {
        let descriptor = setting_descriptor(id);
        if descriptor.scope
            != match target {
                SettingTarget::Global => SettingScope::Global,
                SettingTarget::Input(_) => SettingScope::Input,
            }
        {
            return None;
        }
        Some(match (id, target) {
            (SettingId::BootTarget, SettingTarget::Global) => {
                self.global_settings.boot_target as i32
            }
            (SettingId::RestoreLastTarget, SettingTarget::Global) => {
                self.global_settings.restore_last_target as i32
            }
            (SettingId::AutoReconnect, SettingTarget::Global) => {
                self.global_settings.auto_reconnect as i32
            }
            (SettingId::SwitchReleaseDelayMs, SettingTarget::Global) => {
                self.global_settings.switch_release_delay_ms as i32
            }
            (SettingId::ButtonShortAction, SettingTarget::Global) => {
                self.global_settings.button_short_action as i32
            }
            (SettingId::ButtonLongAction, SettingTarget::Global) => {
                self.global_settings.button_long_action as i32
            }
            (SettingId::ButtonVeryLongAction, SettingTarget::Global) => {
                self.global_settings.button_very_long_action as i32
            }
            (SettingId::LogLevel, SettingTarget::Global) => self.global_settings.log_level as i32,
            (id, SettingTarget::Input(profile_id)) => {
                let settings = self.input_profiles.get(profile_id)?.settings;
                match id {
                    SettingId::KeyboardLayout => settings.keyboard_layout as i32,
                    SettingId::RemapFromUsage => settings.remap_from_usage as i32,
                    SettingId::RemapToUsage => settings.remap_to_usage as i32,
                    SettingId::MouseSensitivityPercent => settings.mouse_sensitivity_percent as i32,
                    SettingId::ScrollMultiplierPercent => settings.scroll_multiplier_percent as i32,
                    SettingId::ConsumerFromUsage => settings.consumer_from_usage as i32,
                    SettingId::ConsumerToUsage => settings.consumer_to_usage as i32,
                    SettingId::TargetSwitchShortcut => {
                        settings.target_switch_shortcut.packed() as i32
                    }
                    _ => return None,
                }
            }
            _ => return None,
        })
    }

    fn set_setting(&mut self, id: SettingId, target: SettingTarget, value: i32) -> bool {
        if !validate_setting_value(id, value) || self.setting_value(id, target).is_none() {
            return false;
        }
        match (id, target) {
            (SettingId::BootTarget, SettingTarget::Global) => {
                self.global_settings.boot_target = value as u8
            }
            (SettingId::RestoreLastTarget, SettingTarget::Global) => {
                self.global_settings.restore_last_target = value != 0
            }
            (SettingId::AutoReconnect, SettingTarget::Global) => {
                self.global_settings.auto_reconnect = value != 0
            }
            (SettingId::SwitchReleaseDelayMs, SettingTarget::Global) => {
                self.global_settings.switch_release_delay_ms = value as u16
            }
            (SettingId::ButtonShortAction, SettingTarget::Global) => {
                self.global_settings.button_short_action = value as u8
            }
            (SettingId::ButtonLongAction, SettingTarget::Global) => {
                self.global_settings.button_long_action = value as u8
            }
            (SettingId::ButtonVeryLongAction, SettingTarget::Global) => {
                self.global_settings.button_very_long_action = value as u8
            }
            (SettingId::LogLevel, SettingTarget::Global) => {
                self.global_settings.log_level = value as u8;
            }
            (id, SettingTarget::Input(profile_id)) => {
                let Some(profile) = self.input_profiles.get_mut(profile_id) else {
                    return false;
                };
                let settings = &mut profile.settings;
                match id {
                    SettingId::KeyboardLayout => settings.keyboard_layout = value as u8,
                    SettingId::RemapFromUsage => settings.remap_from_usage = value as u8,
                    SettingId::RemapToUsage => settings.remap_to_usage = value as u8,
                    SettingId::MouseSensitivityPercent => {
                        settings.mouse_sensitivity_percent = value as u16
                    }
                    SettingId::ScrollMultiplierPercent => {
                        settings.scroll_multiplier_percent = value as u16
                    }
                    SettingId::ConsumerFromUsage => settings.consumer_from_usage = value as u16,
                    SettingId::ConsumerToUsage => settings.consumer_to_usage = value as u16,
                    SettingId::TargetSwitchShortcut => {
                        let Some(shortcut) =
                            crate::settings::KeyboardShortcut::from_packed(value as u16)
                        else {
                            return false;
                        };
                        settings.target_switch_shortcut = shortcut;
                    }
                    _ => return false,
                }
            }
            _ => return false,
        }
        true
    }
}

const fn valid_management_host<const HOSTS: usize>(host_id: HostId) -> bool {
    host_id.0 != 0 && (host_id.0 as usize) <= HOSTS && (host_id.0 as usize) <= 4
}

const fn management_command_requires_storage(command: &ManagementCommand) -> bool {
    match command {
        ManagementCommand::SelectHost(_)
        | ManagementCommand::StartPairing(_)
        | ManagementCommand::ForgetHost(_)
        | ManagementCommand::SetHostName { .. }
        | ManagementCommand::SetSetting { .. } => true,
        #[cfg(feature = "dual-s3-wired")]
        ManagementCommand::SelectOutputTarget(_)
        | ManagementCommand::SetMirrorTarget(_)
        | ManagementCommand::ClearMirrorTarget
        | ManagementCommand::SetWiredHostLink { .. } => true,
        _ => false,
    }
}
