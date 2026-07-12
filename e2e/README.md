# Hardware E2E

The suite uses an ESP32-S3 DUT and a classic ESP32 BLE Central probe, both
connected to Linux over USB. It tests the normalized keyboard, mouse, and
consumer-control boundary through the real BLE path, then verifies a Linux
host's evdev key event. It does not replace USB descriptor/enumeration tests at
the physical USB port.

## Requirements

The Linux user needs access to serial ports and evdev (`dialout` and `input`
groups). Device-specific udev rules are not required: the runner discovers
stable `/dev/serial/by-path` entries and verifies each chip with `espflash`.
Install BlueZ, `bluetoothctl`, `espflash`, and the esp-rs toolchain, then load
the toolchain environment:

```sh
source ~/export-esp.sh
```

## Run

```sh
cargo run --manifest-path e2e/runner/Cargo.toml -- \
  --latency-samples 200 --stability-seconds 120
```

Use `--skip-linux` for Probe-only measurements, `--skip-flash` to reuse loaded
images, or `--dut-port` and `--probe-port` to override discovery.

Timestamped results are written under `e2e/results/`, which is intentionally
ignored by Git. The checked-in `baseline.json` is the reviewable performance
reference. The primary latency metric uses synchronized clocks from the DUT
input boundary to the Probe receive timestamp; `host_observed_latency` also
includes UART and USB-serial delivery. Pipeline fields split runtime
processing, queueing, BLE-task wake-up, GATT dispatch, notification submission,
and over-the-air delivery.

Regenerate the schema-versioned baseline with `--write-baseline` only after a
representative hardware run.
