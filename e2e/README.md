# Direct BLE hardware E2E

The hardware suite measures the path from the DUT's normalized input boundary
through the real BLE stack to an ESP BLE Central probe. It does not replace
physical USB Host descriptor or enumeration testing.

## Requirements

- ESP32-S3 DUT connected over serial
- compatible ESP BLE Central probe connected over serial
- Linux with BlueZ, `bluetoothctl`, `espflash`, and the esp-rs toolchain
- serial and evdev access, normally through the `dialout` and `input` groups

The runner discovers unique ports below `/dev/serial/by-path`, then verifies
chip types with `espflash`. Ports can be supplied explicitly if discovery is
ambiguous.

## Run

```sh
source ~/export-esp.sh
cargo run --manifest-path e2e/runner/Cargo.toml -- \
  --latency-samples 500 \
  --stability-seconds 120
```

Useful options are:

- `--dut-port` and `--probe-port` to override port discovery
- `--skip-flash` with explicit `--dut-port`, `--probe-port`, and `--probe-chip`
  to reuse loaded images without resetting boards during chip discovery
- `--skip-linux` to omit Linux-side integration checks
- `--write-baseline` to replace the checked-in baseline after a representative run

The runner flashes both images, pairs the probe, verifies keyboard, mouse, and
consumer reports, checks reconnect/bond behavior, measures keyboard and mouse
latency, and runs continuous delivery for the requested duration. It also
records connection interval, peripheral latency, PHY, resets, sequence gaps,
and the following pipeline stages:

```text
DUT ingress -> runtime -> BLE queue -> GATT/notify -> HCI -> air -> Probe
```

The primary metric maps DUT and Probe timestamps to the host using independent
minimum-round-trip clock synchronization. Host-observed latency, which includes
USB-serial delivery, is diagnostic only. The low-latency gate requires both
keyboard and mouse p95 below 10 ms and p99 at most 15 ms.

In the default full run, Linux is paired as host 2 before latency collection.
The runner switches back to the Probe and verifies that both links remain
connected and encrypted, so the reported latency and stability cover a
retained two-host session. `--skip-linux` intentionally measures one link.

Timestamped JSON results are written to ignored files in `e2e/results/`.
`e2e/baseline.json` is the only tracked performance reference. Do not update it
from a short or otherwise unrepresentative run.

## Dual-S3 Wired and Mirror E2E

`e2e/mirror-runner` flashes both S3 boards, uses normalized UART injection for
Fallback HID, and registers synthetic `.hsmi` candidates for dynamic Mirror
tests. It verifies Linux evdev, byte-exact Device/Configuration/HID Report
Descriptors, USB strings, raw endpoint reports, profile switching,
invalid-image rejection, BLE/Wired presentation switching, LED output, and
Device S3 reboot recovery without a physical test keyboard. It also drops
Host-side SPI polls long enough to exercise the 1.5-second link-loss path,
asserting that Wired remains selected with no active failover target before
Fallback recovers.

A normal flashing run erases the Host settings partition and Device Mirror
profile partition first, so every run exercises fresh Profile A/B commits.
The dual-S3 hardware-E2E Host uses volatile settings storage to keep its
controller-less BLE test configuration from pausing the 500 µs SPI poll loop;
Device S3 profile flash and reboot persistence are still exercised.

```sh
cargo run --manifest-path e2e/mirror-runner/Cargo.toml -- \
  --host-port /dev/serial/by-id/<host-s3> \
  --device-flash-port /dev/serial/by-id/<device-s3> \
  --ble-address 6A:EE:8F:64:11:AD \
  --linux-controller-address 4C:23:38:A6:20:44
```

Use `--skip-flash` to reuse loaded images. Explicit ports avoid confusing the
two ESP32-S3 roles after Device S3 changes its native USB identity.
`--ble-address` enables the BlueZ BLE Management, HID release/suppression and
no-broadcast cases in `--ble-host-slot` (default 2). Build `tools/hidshiftctl`
in release mode first or override `--hidshiftctl`. A flashing BLE run also
requires the local controller address so the hardware-E2E firmware can
authorize that peer. Because hardware-E2E bond state is volatile, a flashing
run removes the stale BlueZ bond and pairs the peer automatically.
`--skip-flash` preserves and reuses the existing bond unless `--pair-ble` is
also specified.
`--skip-hidraw` omits Vendor and Feature Report cases when local udev policy
does not grant read/write access to `/dev/hidraw*`; evdev cases still run.
Use `--skip-flash --spi-loss-only --device-flash-port <device-s3>` for a short
T26 run against already loaded hardware. This mode does not read or register
Mirror fixture files; the Device port is used only to reset the intentionally
offline SPI slave after the no-failover assertion.
