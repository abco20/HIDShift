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
- `--skip-flash` to reuse loaded images
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

Timestamped JSON results are written to ignored files in `e2e/results/`.
`e2e/baseline.json` is the only tracked performance reference. Do not update it
from a short or otherwise unrepresentative run.
