use bt_hci::param::{AddrKind, BdAddr};
use hidshift::storage::{StorageState, StoredAddressKind, StoredBond, StoredSecurityLevel};
use trouble_host::prelude::{
    BondInformation, Controller, Identity, IdentityResolvingKey, LongTermKey, PacketPool,
    SecurityLevel, Stack,
};

pub(super) fn restore<C, P>(stack: &Stack<'_, C, P>, storage: &StorageState)
where
    C: Controller,
    P: PacketPool,
{
    let mut restored = 0usize;
    for host in storage.hosts().iter() {
        let Some(bond) = host.bond else {
            continue;
        };
        match to_trouble(bond) {
            Some(bond_information) => {
                log::debug!(
                    "firmware: restoring bond host={} identity={:?} bonded={} level={:?}",
                    host.host_id.0,
                    bond_information.identity,
                    bond_information.is_bonded,
                    bond_information.security_level
                );
                if let Err(err) = stack.add_bond_information(bond_information) {
                    log::error!(
                        "firmware: failed to restore bond for host={} err={:?}",
                        host.host_id.0,
                        err
                    );
                } else {
                    restored += 1;
                }
            }
            None => log::warn!("firmware: invalid stored bond for host={}", host.host_id.0),
        }
    }
    log::info!(
        "firmware: restored {} bond(s); stack now has {} bond(s)",
        restored,
        stack.get_bond_information().len()
    );
}

pub(super) fn from_trouble(bond: BondInformation, address_kind: AddrKind) -> StoredBond {
    StoredBond {
        peer_address: bond.identity.bd_addr.into_inner(),
        peer_address_kind: if address_kind == AddrKind::RANDOM {
            StoredAddressKind::Random
        } else {
            StoredAddressKind::Public
        },
        peer_irk: bond.identity.irk.map(|irk| irk.to_le_bytes()),
        ltk: bond.ltk.to_le_bytes(),
        is_bonded: bond.is_bonded,
        security_level: match bond.security_level {
            SecurityLevel::NoEncryption => StoredSecurityLevel::NoEncryption,
            SecurityLevel::Encrypted => StoredSecurityLevel::Encrypted,
            SecurityLevel::EncryptedAuthenticated => StoredSecurityLevel::EncryptedAuthenticated,
        },
    }
}

pub(super) fn to_trouble(bond: StoredBond) -> Option<BondInformation> {
    let identity = Identity {
        bd_addr: BdAddr::new(bond.peer_address),
        irk: bond.peer_irk.map(IdentityResolvingKey::from_le_bytes),
    };
    Some(BondInformation::new(
        identity,
        LongTermKey::from_le_bytes(bond.ltk),
        match bond.security_level {
            StoredSecurityLevel::NoEncryption => SecurityLevel::NoEncryption,
            StoredSecurityLevel::Encrypted => SecurityLevel::Encrypted,
            StoredSecurityLevel::EncryptedAuthenticated => SecurityLevel::EncryptedAuthenticated,
        },
        bond.is_bonded,
    ))
}
