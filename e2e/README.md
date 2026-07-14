# Hardware E2E

Bridge and coexistence modes use two ESP32-S3 boards connected to Linux over
USB: a Host/input-side board runs ESP-NOW (and BLE in coexistence mode), while
a Device/PC-side board exposes native USB HID. BLE-only mode retains the
classic ESP32 Central probe. The suite tests the normalized keyboard, mouse,
consumer-control, and vendor boundaries through real radio and Linux HID paths.
It does not replace descriptor/enumeration tests at the physical USB Host port.

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
  --mode bridge --latency-samples 500 --stability-seconds 120
```

The runner never assigns roles from a board-specific USB serial number or MAC.
For already provisioned firmware it queries the Serial management protocol and
uses the persisted ESP-NOW role. Two identical blank boards have no observable
role, so initial provisioning requires both ports explicitly:

```sh
cargo run --manifest-path e2e/runner/Cargo.toml -- \
  --mode bridge \
  --host-port /dev/serial/by-path/<input-side-board> \
  --device-port /dev/serial/by-path/<pc-side-board> \
  --latency-samples 500 --stability-seconds 120
```

After flashing, the runner reads each local MAC through management, generates a
new key, commits reciprocal pairing records, and resets both boards. Subsequent
runs can discover the persisted roles without explicit ports. Boards with a
USB-UART auto-reset circuit do not require manual BOOT/RESET operation.
Use `--espnow-channel` when channel 6 is unsuitable for the test environment.

In bridge mode, keyboard and mouse latency samples are interleaved and the
measurement order is reversed on alternating rounds. This reduces bias from
time-varying ESP-NOW radio scheduling. The bridge result JSON includes
`keyboard_espnow_tx` and `mouse_espnow_tx` in addition to end-to-end latency.
These fields measure Host ingress through the actual ESP-NOW send callback.
Device-side radio-to-reassembly and HID-write stages are sampled independently
for the critical and motion lanes after the HID write. Only one in eight
reports per lane emits reverse-direction telemetry, keeping diagnostics from
materially competing with realtime input.

The Host path is also split into `ingress_to_enqueue`,
`enqueue_to_dequeue`, `dequeue_to_send_start`, and
`send_start_to_callback`. Hardware telemetry uses a fixed 32-entry sequence
ring, cleared by the runner Hello, so callback completion order cannot replace
the sample being read. Latency injection uses keyboard-only release, drains
stale evdev events, and randomizes the radio phase. Continuous-load behavior
is evaluated separately by the stability test.

Realtime snapshots use encrypted/authenticated 54 Mbps ESP-NOW broadcast.
Critical reports retain an ordered fixed-size transition journal and one
bounded state refresh; motion remains cumulative and coalesced. Functional
tests inject loss to verify idle-tail, transition-history, piggyback, and
cumulative recovery without packet retransmission.

The synthetic bridge descriptor includes a 63-byte vendor input/output/feature
report. In `hardware-e2e,espnow`, a fixed-capacity source-side responder
handles output and feature GET/SET requests so the complete reverse ESP-NOW
path can be exercised without enabling the physical USB Host stack. A motion
burst and mixed keyboard/mouse/consumer rounds cover queue pressure,
coalescing, ordering, and stuck-state recovery.

Coexistence keeps BLE at a 7.5 ms connection interval, 2M PHY, and Peripheral
Latency 19. Route selection changes immediately and does not renegotiate
connection parameters. E2E verifies the negotiated values, pair/reconnect,
bond persistence, and route isolation. ESP-NOW must satisfy p95 below 10 ms and
p99 at most 15 ms; BLE must satisfy p95 at most 15 ms.

Use `--skip-linux` for Probe-only measurements, `--skip-flash` to reuse loaded
images, or the mode-specific port options to override discovery.

Timestamped results are written under `e2e/results/`, which is intentionally
ignored by Git. The checked-in `baseline.json` is the reviewable performance
reference. The primary latency metric uses synchronized clocks from the DUT
input boundary to the Probe receive timestamp; `host_observed_latency` also
includes UART and USB-serial delivery. Pipeline fields split runtime
processing, queueing, BLE-task wake-up, GATT dispatch, notification submission,
and over-the-air delivery.

Regenerate the schema-versioned baseline with `--write-baseline` only after a
representative hardware run.
