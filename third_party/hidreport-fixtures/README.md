Source: `https://github.com/hidutils/hidreport`

License: MIT. See [LICENSE](LICENSE).

Copyright © 2024 Red Hat, Inc.

These files are copied from the upstream `tests/data/` corpus to exercise
public HID descriptors against this repository's host-side parsing and
capability extraction.

Imported subset:

- `0003-045E-00DB.0003.hid.bin`
- `0003-045E-00DB.0004.hid.bin`
- `0003-045E-0024.0004.hid.bin`
- `0003-045E-0745.0002.hid.bin`
- `0003-045E-07A9.000E.hid.bin`
- `libinput-issue510-0005-057E-0306-0.rdesc`

Rationale:

- first four cover keyboard / mouse / consumer / LED / wheel / AC Pan paths
- last two are treated as unsupported fixtures that must parse without panic
  and remain safely ignored by the bridge capability layer
