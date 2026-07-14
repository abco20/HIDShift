use crate::espnow_pairing::{EspNowPairing, EspNowPairingTransaction, EspNowRole};
use crate::management::{
    ManagementCommand, ManagementEspNowInfo, ManagementResponsePayload, ManagementResult,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EspNowPairingAction {
    None,
    Persist(EspNowPairing),
    Clear,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EspNowPairingOutcome {
    pub result: ManagementResult,
    pub payload: ManagementResponsePayload,
    pub action: EspNowPairingAction,
}

pub struct EspNowPairingService {
    role: EspNowRole,
    current: Option<EspNowPairing>,
    transaction: EspNowPairingTransaction,
}

impl EspNowPairingService {
    pub const fn new(role: EspNowRole, current: Option<EspNowPairing>) -> Self {
        Self {
            role,
            current,
            transaction: EspNowPairingTransaction::new(role),
        }
    }

    pub fn handle(
        &mut self,
        command: ManagementCommand,
        local_address: [u8; 6],
    ) -> EspNowPairingOutcome {
        let (result, payload, action) = match command {
            ManagementCommand::GetEspNowInfo => (
                ManagementResult::Ok,
                ManagementResponsePayload::EspNowInfo(ManagementEspNowInfo {
                    paired: self.current.is_some(),
                    role: self.role,
                    channel: self.current.map_or(0, |pairing| pairing.channel),
                    local_address,
                    peer_address: self.current.map_or([0; 6], |pairing| pairing.peer_address),
                }),
                EspNowPairingAction::None,
            ),
            ManagementCommand::BeginEspNowPairing {
                peer_address,
                channel,
            } => (
                map_pairing_result(self.transaction.begin(peer_address, channel)),
                ManagementResponsePayload::None,
                EspNowPairingAction::None,
            ),
            ManagementCommand::WriteEspNowKey {
                offset,
                length,
                bytes,
            } if usize::from(length) <= bytes.len() => (
                map_pairing_result(
                    self.transaction
                        .write_key_chunk(offset, &bytes[..usize::from(length)]),
                ),
                ManagementResponsePayload::None,
                EspNowPairingAction::None,
            ),
            ManagementCommand::CommitEspNowPairing => {
                let generation = self
                    .current
                    .map_or(1, |pairing| pairing.generation.wrapping_add(1));
                match self.transaction.commit(generation) {
                    Ok(pairing) => (
                        ManagementResult::Ok,
                        ManagementResponsePayload::None,
                        EspNowPairingAction::Persist(pairing),
                    ),
                    Err(_) => (
                        ManagementResult::InvalidPairing,
                        ManagementResponsePayload::None,
                        EspNowPairingAction::None,
                    ),
                }
            }
            ManagementCommand::ForgetEspNowPeer => (
                ManagementResult::Ok,
                ManagementResponsePayload::None,
                EspNowPairingAction::Clear,
            ),
            _ => (
                ManagementResult::Unavailable,
                ManagementResponsePayload::None,
                EspNowPairingAction::None,
            ),
        };
        EspNowPairingOutcome {
            result,
            payload,
            action,
        }
    }

    pub fn persisted(&mut self, pairing: EspNowPairing) {
        self.current = Some(pairing);
    }

    pub fn cleared(&mut self) {
        self.current = None;
        self.transaction.cancel();
    }
}

fn map_pairing_result(
    result: Result<(), crate::espnow_pairing::EspNowPairingError>,
) -> ManagementResult {
    result.map_or(ManagementResult::InvalidPairing, |_| ManagementResult::Ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_exposes_no_key_and_commits_only_complete_transactions() {
        let mut service = EspNowPairingService::new(EspNowRole::UsbHost, None);
        let info = service.handle(ManagementCommand::GetEspNowInfo, [1; 6]);
        assert!(matches!(
            info.payload,
            ManagementResponsePayload::EspNowInfo(ManagementEspNowInfo { paired: false, .. })
        ));
        service.handle(
            ManagementCommand::BeginEspNowPairing {
                peer_address: [2; 6],
                channel: 6,
            },
            [1; 6],
        );
        assert_eq!(
            service
                .handle(ManagementCommand::CommitEspNowPairing, [1; 6])
                .result,
            ManagementResult::InvalidPairing
        );
    }

    #[test]
    fn complete_transaction_is_persisted_before_becoming_current() {
        let mut service = EspNowPairingService::new(EspNowRole::UsbDevice, None);
        let local_address = [1; 6];
        assert_eq!(
            service
                .handle(
                    ManagementCommand::BeginEspNowPairing {
                        peer_address: [2; 6],
                        channel: 6,
                    },
                    local_address,
                )
                .result,
            ManagementResult::Ok
        );
        for (offset, bytes) in [(0, &[1; 14][..]), (14, &[2; 2][..])] {
            assert_eq!(
                service
                    .handle(
                        ManagementCommand::WriteEspNowKey {
                            offset,
                            length: bytes.len() as u8,
                            bytes: {
                                let mut encoded = [0; 14];
                                encoded[..bytes.len()].copy_from_slice(bytes);
                                encoded
                            },
                        },
                        local_address,
                    )
                    .result,
                ManagementResult::Ok
            );
        }

        let outcome = service.handle(ManagementCommand::CommitEspNowPairing, local_address);
        let EspNowPairingAction::Persist(pairing) = outcome.action else {
            panic!("complete pairing must request persistence");
        };
        assert_eq!(pairing.generation, 1);
        assert_eq!(pairing.local_role, EspNowRole::UsbDevice);

        let before_persist = service.handle(ManagementCommand::GetEspNowInfo, local_address);
        assert!(matches!(
            before_persist.payload,
            ManagementResponsePayload::EspNowInfo(ManagementEspNowInfo { paired: false, .. })
        ));
        service.persisted(pairing);
        let after_persist = service.handle(ManagementCommand::GetEspNowInfo, local_address);
        assert!(matches!(
            after_persist.payload,
            ManagementResponsePayload::EspNowInfo(ManagementEspNowInfo {
                paired: true,
                channel: 6,
                peer_address: [2, 2, 2, 2, 2, 2],
                ..
            })
        ));
    }
}
