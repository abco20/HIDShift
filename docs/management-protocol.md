# HIDShift management protocol v1

This protocol is under development. Compatibility is not guaranteed until the
format is declared stable.

The product-level v1 envelope is variable length and transport neutral. Its
header contains magic `HS`, protocol version, frame kind, 16-bit request ID,
node ID, flags, and payload length; CRC-16/CCITT-FALSE follows the payload.
`src/management/frame.rs` is the host-tested envelope codec. Debug UART can use
COBS plus a zero delimiter. The envelope is designed for a BLE adapter
to fragment the same logical frame at the ATT boundary.

The shipping request/response adapter accepts fixed 20-byte v1 messages during
the firmware transition. Its status payload distinguishes a retrying/degraded
flash backend from healthy persistent storage. The user-facing
wired transport is a vendor-defined HID interface (`usage page 0xff60`, `usage
0x61`) with report IDs `0x10` request, `0x11` response, and `0x12` event. UART
hexadecimal `@HIDSHIFT:` and `@HIDSHIFT-EVENT:` lines are reserved for debugging
and hardware E2E.

## GATT service

| Item | UUID | Properties |
| --- | --- | --- |
| Service | `7f510000-1b15-4f0d-9f4b-5b6d4f3a0001` | — |
| Request | `7f510001-1b15-4f0d-9f4b-5b6d4f3a0001` | write, write without response, encrypted |
| Response | `7f510002-1b15-4f0d-9f4b-5b6d4f3a0001` | read, notify, encrypted |
| Event | `7f510003-1b15-4f0d-9f4b-5b6d4f3a0001` | notify, encrypted |

Clients subscribe before writing. Byte 0 is version 1, byte 1 is the client
request ID, byte 2 is opcode/result, and subsequent bytes contain a length and
typed payload. `src/management.rs` is the authoritative codec and rejects
unknown versions, opcodes, types, lengths, setting IDs, and scopes.

Supported command families are status, select, pairing start/cancel, forget,
host info/name/timing, USB device chunks, diagnostics, history, schema, and
setting get/set. Response payloads are explicitly tagged and never inferred
from an opcode. Result codes distinguish invalid host, missing host, existing
bond, invalid name, invalid setting, missing indexed item, and internal errors.
USB device payloads include the input profile ID used as the target for
input-scoped settings. Only connected USB devices are enumerated; profiles are
retained independently for reconnection.

Capability bit 1 (`companion_events`) exposes the Event characteristic and
`GET_CLIENT_SESSION`. A status event contains a wrapping 16-bit sequence and
only invalidates the client's snapshot; clients fetch status again before
showing a route-change notification. The same event is emitted as an HID input
report, so a USB-connected Companion does not poll for normal changes. Initial
synchronization and reconnection establish a silent baseline.

When schema capability bit 0 (`dual_s3_wired`) is set, clients may also use
`SELECT_OUTPUT_TARGET`, `GET_OUTPUT_TARGET_STATUS`, `GET_MIRROR_CANDIDATE`,
`SET_MIRROR_TARGET`, `CLEAR_MIRROR_TARGET`, and `FORCE_FALLBACK`.

Output and Mirror selections are independent persisted values. Selecting BLE
keeps the Mirror target but presents neutral Fallback USB. No command performs
automatic failover. Mirror operations are asynchronous; status reports
selected/active targets, availability, presentation, and operation ID.

The vendor HID management collection is part of the standard HIDShift fallback
presentation. An exact mirrored presentation intentionally does not append a
HIDShift interface; management remains available through Host S3 BLE while it
is active.

`src/settings.rs` declares the settings schema once. CLI and Web use the same
compiled descriptors and verify the firmware schema version/count/hash before
showing values. Wire IDs are stable numeric values; display labels and command
keys are not used as persistent identifiers.

## Product domains and synchronization

Input profiles are part of the normal core and retain settings for up to eight
physical USB devices. Input setting targets are profile IDs, not BLE destination
slots. Setting 16 stores a packed target-switch shortcut (low byte keyboard
usage, high byte modifier mask) in the profile record without changing storage
schema v1. The remaining `experimental-domain` types cover future destination and
firmware-update synchronization work.

The revision codec represents changes with a domain mask and wrapping revisions
for summary, sessions, destinations, inputs, system, wired, and support. The Web
client already loads a summary first and fetches details lazily; firmware event
delivery and revision-driven refetch remain adapter work.
