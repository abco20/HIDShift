use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use hidshift::espnow_pairing::EspNowRole;

pub fn assign_bridge_roles(
    discovered: impl IntoIterator<Item = (PathBuf, EspNowRole)>,
) -> Result<(PathBuf, PathBuf)> {
    let mut host = None;
    let mut device = None;
    for (path, role) in discovered {
        let slot = match role {
            EspNowRole::UsbHost => &mut host,
            EspNowRole::UsbDevice => &mut device,
        };
        ensure!(
            slot.is_none(),
            "multiple running boards report the {role:?} role"
        );
        *slot = Some(path);
    }
    let host = host.context(
        "no running ESP-NOW Host role found; provide both --host-port and --device-port for initial provisioning",
    )?;
    let device = device.context(
        "no running ESP-NOW Device role found; provide both --host-port and --device-port for initial provisioning",
    )?;
    Ok((host, device))
}

pub fn serial_by_path_candidates(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();
    for entry in fs::read_dir(directory).with_context(|| format!("read {}", directory.display()))? {
        let path = entry?.path();
        let target = fs::canonicalize(&path)?;
        if seen.insert(target.clone()) {
            // espflash follows the by-path symlink for display but cannot
            // reliably open it on all serial backends. Keep the canonical
            // character device after using by-path for deterministic
            // discovery and de-duplication.
            candidates.push(target);
        }
    }
    candidates.sort();
    Ok(candidates)
}

pub fn parse_chip_type(output: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Chip type:")
            .and_then(|rest| rest.split_whitespace().next())
            .map(str::to_owned)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::fs::symlink;

    #[test]
    fn chip_parser_requires_espflash_chip_type_line() {
        assert_eq!(
            parse_chip_type("Chip type:         esp32s3 (revision v0.2)"),
            Some("esp32s3".into())
        );
        assert_eq!(parse_chip_type("Connecting..."), None);
    }

    #[test]
    fn bridge_roles_come_from_management_not_hardware_identity() {
        let host = PathBuf::from("/dev/serial/by-path/board-a");
        let device = PathBuf::from("/dev/serial/by-path/board-b");
        assert_eq!(
            assign_bridge_roles([
                (device.clone(), EspNowRole::UsbDevice),
                (host.clone(), EspNowRole::UsbHost),
            ])
            .unwrap(),
            (host, device)
        );
        assert!(assign_bridge_roles([]).is_err());
        assert!(
            assign_bridge_roles([
                (PathBuf::from("board-a"), EspNowRole::UsbHost),
                (PathBuf::from("board-b"), EspNowRole::UsbHost),
            ])
            .is_err()
        );
    }

    #[test]
    fn by_path_discovery_deduplicates_kernel_devices() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("ttyACM0");
        File::create(&target).unwrap();
        symlink(&target, directory.path().join("physical")).unwrap();
        symlink(&target, directory.path().join("usbv2-alias")).unwrap();
        let candidates = serial_by_path_candidates(directory.path()).unwrap();
        assert_eq!(candidates.len(), 1);
    }
}
