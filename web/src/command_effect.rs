use hidshift::{ManagementCommand, ManagementResponsePayload};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandEffect {
    Status(hidshift::ManagementStatus),
    OutputTarget(hidshift::ManagementOutputTargetStatus),
}

pub(crate) fn immediate_command_effect(
    command: ManagementCommand,
    payload: ManagementResponsePayload,
) -> Option<CommandEffect> {
    match (command, payload) {
        // The single-S3 runtime intentionally waits for the release-report
        // grace period after acknowledging the command. Reflect the accepted
        // selection while preserving the rest of its returned status snapshot.
        (ManagementCommand::SelectHost(host), ManagementResponsePayload::Status(mut status)) => {
            status.active_host = Some(host);
            Some(CommandEffect::Status(status))
        }
        (
            ManagementCommand::SelectOutputTarget(_),
            ManagementResponsePayload::OutputTargetStatus(status),
        ) => Some(CommandEffect::OutputTarget(status)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use hidshift::{
        HostId, ManagementOutputTarget, ManagementOutputTargetStatus, ManagementStatus,
        ManagementUsbPresentationKind, OutputTargetAvailability,
    };

    use super::*;

    #[test]
    fn selected_ble_host_uses_status_from_the_command_response() {
        let status = ManagementStatus::empty(4);
        let mut expected = status;
        expected.active_host = Some(HostId(2));

        assert_eq!(
            immediate_command_effect(
                ManagementCommand::SelectHost(HostId(2)),
                ManagementResponsePayload::Status(status),
            ),
            Some(CommandEffect::Status(expected))
        );
    }

    #[test]
    fn selected_dual_s3_target_uses_output_status_from_the_command_response() {
        let status = ManagementOutputTargetStatus {
            selected: ManagementOutputTarget::Wired,
            active: Some(ManagementOutputTarget::Wired),
            availability: OutputTargetAvailability::Ready,
            wired_ready: true,
            ready_ble_mask: 0b0011,
            effective_presentation: ManagementUsbPresentationKind::Fallback,
            mirror_configured: false,
            operation_id: 7,
        };

        assert_eq!(
            immediate_command_effect(
                ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Wired),
                ManagementResponsePayload::OutputTargetStatus(status),
            ),
            Some(CommandEffect::OutputTarget(status))
        );
    }

    #[test]
    fn unrelated_commands_keep_their_existing_refresh_path() {
        assert_eq!(
            immediate_command_effect(
                ManagementCommand::ForgetHost(HostId(1)),
                ManagementResponsePayload::Status(ManagementStatus::empty(4)),
            ),
            None
        );
    }
}
