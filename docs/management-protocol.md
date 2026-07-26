# HIDShift management protocol v1

This protocol is under development. Compatibility is not guaranteed until the
format is declared stable.

BLE carries binary request and response values. UART carries exactly the same
values as hexadecimal, prefixed with `@HIDSHIFT:` and terminated by a newline.
Both envelopes are fixed at 20 bytes so one message fits the default 23-byte
BLE ATT MTU. Longer names and lists use indexed/chunked requests.

## GATT service

| Item | UUID | Properties |
| --- | --- | --- |
| Service | `7f510000-1b15-4f0d-9f4b-5b6d4f3a0001` | — |
| Request | `7f510001-1b15-4f0d-9f4b-5b6d4f3a0001` | write, write without response, encrypted |
| Response | `7f510002-1b15-4f0d-9f4b-5b6d4f3a0001` | read, notify, encrypted |

Clients subscribe before writing. Byte 0 is version 1, byte 1 is the client
request ID, byte 2 is opcode/result, and subsequent bytes contain a length and
typed payload. `src/management.rs` is the authoritative codec and rejects
unknown versions, opcodes, types, lengths, setting IDs, and scopes.

Supported command families are status, select, pairing start/cancel, forget,
host info/name/timing, USB device chunks, diagnostics, history, schema, and
setting get/set. Response payloads are explicitly tagged and never inferred
from an opcode. Result codes distinguish invalid host, missing host, existing
bond, invalid name, invalid setting, missing indexed item, and internal errors.

When schema capability bit 0 (`dual_s3_wired`) is set, clients may also use
`SELECT_OUTPUT_TARGET`, `GET_OUTPUT_TARGET_STATUS`, `GET_MIRROR_CANDIDATE`,
`SET_MIRROR_TARGET`, `CLEAR_MIRROR_TARGET`, and `FORCE_FALLBACK`.

Output and Mirror selections are independent persisted values. Selecting BLE
keeps the Mirror target but presents neutral Fallback USB. No command performs
automatic failover. Mirror operations are asynchronous; status reports
selected/active targets, availability, presentation, and operation ID.

These commands remain available through Host S3 BLE Management while Device
S3 presents mirrored USB. Device S3 exposes no management, serial, CDC, or
vendor interface on its native USB connection.

`src/settings.rs` declares the settings schema once. CLI and Web use the same
compiled descriptors and verify the firmware schema version/count/hash before
showing values. Wire IDs are stable numeric values; display labels and command
keys are not used as persistent identifiers.
