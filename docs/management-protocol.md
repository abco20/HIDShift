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

`src/settings.rs` declares the settings schema once. CLI and Web use the same
compiled descriptors and verify the firmware schema version/count/hash before
showing values. Wire IDs are stable numeric values; display labels and command
keys are not used as persistent identifiers.
