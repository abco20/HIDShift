# HIDShift

HIDShift is Rust firmware for an ESP32-S3 USB Host that forwards keyboards,
mice, and consumer controls to the currently selected Bluetooth Low Energy
host. Up to four BLE host sessions can remain registered so the active target
can change without first disconnecting the previous host.

The default image remains a one-board BLE bridge. The optional
`dual-s3-wired` image adds a second ESP32-S3 as a native USB Device, allowing
exclusive switching between Wired USB and BLE hosts and dynamic mirroring of
one USB HID device.

## Hardware

- ESP32-S3
- USB OTG D- on GPIO19 and D+ on GPIO20
- Active-low control button on GPIO0
- A stable 5 V supply suitable for the attached devices and optional hub

Direct HID devices, composite devices, and one level of USB hub are supported.
Optional dual-S3 wiring and behavior are documented in
[docs/dual-s3.md](docs/dual-s3.md).

## Build and flash

Install mise, then install the project-managed host and Web tools:

```sh
mise install
```

This installs Rust 1.95.0 with rustfmt, clippy, and the
`wasm32-unknown-unknown` target, plus Trunk, espflash, and espup. To develop
firmware, install the ESP32-S3 Xtensa Rust toolchain separately:

```sh
mise run esp:install
```

Build and flash the production image with:

```sh
mise run firmware:build
mise run firmware:flash
```

`firmware:flash` builds first, flashes the production image, and uses automatic
serial-port detection. It does not start a monitor. Firmware tasks use the
project-local ESP environment automatically, so no shell profile changes are
required.

The normal task builds the one-board image. Build and flash the optional
dual-S3 pair with:

```sh
mise run firmware:build-dual
HIDSHIFT_HOST_PORT=<HOST_UART> mise run firmware:flash-dual
mise run device-firmware:build
HIDSHIFT_DEVICE_PORT=<DEVICE_UART> mise run device-firmware:flash
```

`dual-s3-wired` is absent from the production one-board build.
`hardware-e2e` adds test injection only and must not be used in production.

On Linux, espflash may require the system packages `libudev-dev` and
`pkg-config`. Install OS packages separately; mise tasks do not run `sudo` or
install system dependencies automatically.

## Controls

Actions occur when GPIO0 is released.

| Button hold | Action |
| --- | --- |
| Less than 3 seconds | Select the next ready output |
| 3 to 8 seconds | Pair the next available host slot |
| 8 seconds or longer | Remove the active host bond |

The pairing window remains open for 60 seconds or until pairing succeeds.
Target switching does not wait for the old host to disconnect. The keyboard
report is boot-compatible 6KRO; keys beyond the six-key limit are ignored until
released.

The one-board image cycles ready BLE hosts. The dual-S3 image cycles Wired,
then ready BLE hosts 1–4, skipping unavailable targets.

## Management

Management is available over BLE and serial. It covers status, target
selection, pairing, bond removal, diagnostics, and persistent settings. The
GPIO0 button remains a fallback.

With dual-S3 firmware, BLE management also selects Wired/BLE output and the
Mirror target. Device S3 intentionally exposes HID only, never CDC or a
management interface. BLE management therefore remains available while a
mirrored USB device is attached or re-enumerating.

- `tools/hidshiftctl`: command-line client
- `web`: Web Bluetooth / Web Serial client

Build and use the CLI as follows:

```sh
cargo build --release --manifest-path tools/hidshiftctl/Cargo.toml
tools/hidshiftctl/target/release/hidshiftctl --serial <PORT> status
tools/hidshiftctl/target/release/hidshiftctl --ble status
tools/hidshiftctl/target/release/hidshiftctl --ble pair 2
tools/hidshiftctl/target/release/hidshiftctl --ble target usb
tools/hidshiftctl/target/release/hidshiftctl --ble mirror list
tools/hidshiftctl/target/release/hidshiftctl --ble mirror select 0
```

When no BLE address is supplied, the CLI scans for `HIDShift`. The protocol is
still free to make breaking changes; its current definition is in
[docs/management-protocol.md](docs/management-protocol.md).

The Web UI can be served locally with mise and Trunk:

```sh
mise run web:serve
```

## Development

Core bridge logic is `no_std` and host-tested. Run these checks before a
change is merged. The single mise task runs the host CI checks in the same
order as GitHub Actions:

```sh
mise run host:ci
```

For a local Web production build, run `mise run web:build`. Host tests and Web
development do not require `mise run esp:install`. Firmware validation is
available with `mise run firmware:check`.

GitHub Actions continues to install and select its toolchains directly; it is
not managed by mise.

The direct-BLE hardware suite is described in [e2e/README.md](e2e/README.md).
It verifies reports, reconnect and bond behavior, sustained delivery, and
latency. The direct input-to-BLE target is keyboard and mouse p95 below 10 ms
and p99 at most 15 ms. Hardware and RF conditions affect individual results;
timestamped runs are ignored while `e2e/baseline.json` is the reviewable
reference.

## Releases

Tagged releases contain one ESP32-S3 production image plus `hidshiftctl` for
Linux, Windows, and macOS. The firmware archive contains:

- `hidshift.elf` for normal `espflash flash` use
- `hidshift-factory.bin`, a merged image starting at address `0x0`
- the partition table, README, project license, and dependency license report

Factory images assume at least 4 MiB of flash. Release tags use `vX.Y.Z` or a
prerelease suffix such as `vX.Y.Z-rc.1`; package versions and `CHANGELOG.md`
must match the tag.

## Logs

Production builds include `info` and higher severity logs. Override the
compile-time filter when diagnosing a problem:

```sh
ESP_LOG=debug mise run firmware:build
```

## License

HIDShift is licensed under the MIT License. See [LICENSE](LICENSE) and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
