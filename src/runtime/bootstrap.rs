use crate::bridge::BridgeEvent;
use crate::ids::HostId;
use crate::reports::ReportKind;
use crate::storage::StorageState;

use super::{BridgeRuntime, RuntimeCommand, RuntimeError};

pub fn prepare_ready_host<
    const HOSTS: usize,
    const USB_KEYBOARDS: usize,
    const COMMANDS: usize,
    const ACTIONS: usize,
    const EVENTS: usize,
>(
    runtime: &mut BridgeRuntime<HOSTS, USB_KEYBOARDS>,
    host_id: HostId,
    reports: &[ReportKind],
    commands: &mut heapless::Vec<RuntimeCommand, COMMANDS>,
) -> Result<(), RuntimeError> {
    for event in [
        BridgeEvent::HostConnected { host_id },
        BridgeEvent::HostSecurityChanged {
            host_id,
            encrypted: true,
            bonded: true,
            bond: None,
        },
    ] {
        runtime.handle_input::<COMMANDS, ACTIONS, EVENTS>(
            super::RuntimeInput::BridgeEvent(event),
            commands,
        )?;
    }

    for report in reports.iter().copied() {
        runtime.handle_input::<COMMANDS, ACTIONS, EVENTS>(
            super::RuntimeInput::BridgeEvent(BridgeEvent::CccdChanged {
                host_id,
                report,
                enabled: true,
            }),
            commands,
        )?;
    }

    runtime.handle_input::<COMMANDS, ACTIONS, EVENTS>(
        super::RuntimeInput::BridgeEvent(BridgeEvent::SwitchTarget { target: host_id }),
        commands,
    )?;
    Ok(())
}

pub fn storage_with_default_target(storage: &StorageState, default_host: HostId) -> StorageState {
    let mut restored = storage.clone();
    if storage.last_active_host.is_some() {
        return restored;
    }

    restored.last_active_host = Some(default_host);
    restored
}

pub fn initial_pairing_host(storage: &StorageState, default_host: HostId) -> Option<HostId> {
    if storage.hosts().is_empty() {
        Some(default_host)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::BridgeStatus;
    use crate::runtime::RuntimeCommand;

    #[test]
    fn prepare_ready_host_marks_target_ready_for_all_requested_reports() {
        let mut runtime = BridgeRuntime::<2, 0>::new(0);
        let mut commands = heapless::Vec::<RuntimeCommand, 8>::new();

        prepare_ready_host::<2, 0, 8, 8, 2>(
            &mut runtime,
            HostId(1),
            &[
                ReportKind::Keyboard,
                ReportKind::Mouse,
                ReportKind::Consumer,
            ],
            &mut commands,
        )
        .unwrap();

        assert!(runtime.bridge().can_send(HostId(1), ReportKind::Keyboard));
        assert!(runtime.bridge().can_send(HostId(1), ReportKind::Mouse));
        assert!(runtime.bridge().can_send(HostId(1), ReportKind::Consumer));
        assert!(matches!(
            commands.last(),
            Some(RuntimeCommand::StatusChanged(BridgeStatus {
                active_target: Some(HostId(1)),
                pairable_host: None,
            }))
        ));
    }

    #[test]
    fn default_target_is_selected_only_when_storage_has_no_active_host() {
        let storage = StorageState::new(10);

        let restored = storage_with_default_target(&storage, HostId(1));

        assert_eq!(restored.last_active_host, Some(HostId(1)));
        assert_eq!(restored.generation, 10);
    }

    #[test]
    fn saved_active_host_is_not_replaced_by_default_target() {
        let mut storage = StorageState::new(10);
        storage.last_active_host = Some(HostId(2));

        let restored = storage_with_default_target(&storage, HostId(1));

        assert_eq!(restored.last_active_host, Some(HostId(2)));
        assert_eq!(restored.generation, 10);
    }

    #[test]
    fn empty_storage_opens_initial_pairing_for_default_host() {
        let storage = StorageState::new(10);

        assert_eq!(initial_pairing_host(&storage, HostId(1)), Some(HostId(1)));
    }

    #[test]
    fn stored_host_profiles_disable_automatic_initial_pairing() {
        let mut storage = StorageState::new(10);
        storage
            .push_host(crate::storage::StoredHostProfile::empty())
            .unwrap();

        assert_eq!(initial_pairing_host(&storage, HostId(1)), None);
    }
}
