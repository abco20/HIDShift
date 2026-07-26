use std::fs;
use std::path::PathBuf;

use hidshift::fallback::{KEYBOARD_REPORT_DESCRIPTOR, MOUSE_REPORT_DESCRIPTOR};
use hidshift::mirror::{
    HSMI_MAX_SIZE, HidReportRecord, MirrorImageSource, StringRecord, serialize_mirror_image,
    validate_mirror_image,
};

const VENDOR_REPORT: &[u8] = &[
    0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x10, 0x09, 0x02, 0x15, 0x00, 0x26, 0xff, 0x00,
    0x75, 0x08, 0x95, 0x3f, 0x81, 0x02, 0x09, 0x03, 0x95, 0x3f, 0x91, 0x02, 0x09, 0x04, 0x95, 0x10,
    0xb1, 0x02, 0xc0,
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("e2e/fixtures/mirror"));
    fs::create_dir_all(&output)?;

    write_fixture(
        output.join("composite-a.hsmi"),
        0x4001,
        0x0100,
        "Dynamic Composite A",
        "E2E-A",
        &composite_configuration(),
        &[
            HidReportRecord {
                interface_number: 0,
                descriptor: KEYBOARD_REPORT_DESCRIPTOR,
            },
            HidReportRecord {
                interface_number: 1,
                descriptor: VENDOR_REPORT,
            },
        ],
    )?;
    write_fixture(
        output.join("mouse-b.hsmi"),
        0x4002,
        0x0101,
        "Dynamic Mouse B",
        "E2E-B",
        &mouse_configuration(),
        &[HidReportRecord {
            interface_number: 0,
            descriptor: MOUSE_REPORT_DESCRIPTOR,
        }],
    )?;
    write_fixture(
        output.join("invalid-duplicate-endpoint.hsmi"),
        0x40ff,
        0x0100,
        "Invalid Duplicate Endpoint",
        "E2E-X",
        &invalid_configuration(),
        &[
            HidReportRecord {
                interface_number: 0,
                descriptor: KEYBOARD_REPORT_DESCRIPTOR,
            },
            HidReportRecord {
                interface_number: 1,
                descriptor: MOUSE_REPORT_DESCRIPTOR,
            },
        ],
    )?;
    Ok(())
}

fn write_fixture(
    path: PathBuf,
    product_id: u16,
    release: u16,
    product: &str,
    serial: &str,
    configuration: &[u8],
    reports: &[HidReportRecord<'_>],
) -> Result<(), Box<dyn std::error::Error>> {
    let manufacturer = usb_string("HIDShift E2E")?;
    let product = usb_string(product)?;
    let serial = usb_string(serial)?;
    let language = [4, 3, 0x09, 0x04];
    let strings = [
        StringRecord {
            index: 0,
            lang_id: 0,
            descriptor: &language,
        },
        StringRecord {
            index: 1,
            lang_id: 0x0409,
            descriptor: &manufacturer,
        },
        StringRecord {
            index: 2,
            lang_id: 0x0409,
            descriptor: &product,
        },
        StringRecord {
            index: 3,
            lang_id: 0x0409,
            descriptor: &serial,
        },
    ];
    let [pid_low, pid_high] = product_id.to_le_bytes();
    let [release_low, release_high] = release.to_le_bytes();
    let device = [
        18,
        1,
        0x00,
        0x02,
        0,
        0,
        0,
        64,
        0xfe,
        0xca,
        pid_low,
        pid_high,
        release_low,
        release_high,
        1,
        2,
        3,
        1,
    ];
    let mut image = [0; HSMI_MAX_SIZE];
    let length = serialize_mirror_image(
        MirrorImageSource {
            flags: 0,
            device_descriptor: &device,
            configuration_descriptor: configuration,
            bos_descriptor: &[],
            strings: &strings,
            hid_reports: reports,
        },
        &mut image,
    )
    .map_err(|error| std::io::Error::other(format!("fixture encode failed: {error:?}")))?;
    if !path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with("invalid-"))
    {
        validate_mirror_image(&image[..length])
            .map_err(|error| std::io::Error::other(format!("fixture invalid: {error:?}")))?;
    }
    fs::write(path, &image[..length])?;
    Ok(())
}

fn usb_string(value: &str) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let encoded: Vec<u16> = value.encode_utf16().collect();
    let length = encoded.len() * 2 + 2;
    if length > u8::MAX as usize {
        return Err("USB string too long".into());
    }
    let mut descriptor = Vec::with_capacity(length);
    descriptor.extend_from_slice(&[length as u8, 3]);
    for character in encoded {
        descriptor.extend_from_slice(&character.to_le_bytes());
    }
    Ok(descriptor)
}

fn composite_configuration() -> Vec<u8> {
    let mut config = configuration_header(73, 2);
    append_hid_interface(
        &mut config,
        0,
        1,
        1,
        KEYBOARD_REPORT_DESCRIPTOR.len(),
        &[(0x81, 8, 1), (0x01, 1, 1)],
    );
    append_hid_interface(
        &mut config,
        1,
        0,
        0,
        VENDOR_REPORT.len(),
        &[(0x82, 64, 1), (0x02, 64, 1)],
    );
    config
}

fn mouse_configuration() -> Vec<u8> {
    let mut config = configuration_header(34, 1);
    append_hid_interface(
        &mut config,
        0,
        0,
        2,
        MOUSE_REPORT_DESCRIPTOR.len(),
        &[(0x83, 8, 2)],
    );
    config
}

fn invalid_configuration() -> Vec<u8> {
    let mut config = configuration_header(59, 2);
    append_hid_interface(
        &mut config,
        0,
        1,
        1,
        KEYBOARD_REPORT_DESCRIPTOR.len(),
        &[(0x81, 8, 1)],
    );
    append_hid_interface(
        &mut config,
        1,
        0,
        2,
        MOUSE_REPORT_DESCRIPTOR.len(),
        &[(0x81, 8, 1)],
    );
    config
}

fn configuration_header(total_length: u16, interface_count: u8) -> Vec<u8> {
    let [low, high] = total_length.to_le_bytes();
    vec![9, 2, low, high, interface_count, 1, 0, 0x80, 50]
}

fn append_hid_interface(
    config: &mut Vec<u8>,
    number: u8,
    subclass: u8,
    protocol: u8,
    report_length: usize,
    endpoints: &[(u8, u16, u8)],
) {
    config.extend_from_slice(&[
        9,
        4,
        number,
        0,
        endpoints.len() as u8,
        3,
        subclass,
        protocol,
        0,
    ]);
    let [report_low, report_high] = (report_length as u16).to_le_bytes();
    config.extend_from_slice(&[9, 0x21, 0x11, 0x01, 0, 1, 0x22, report_low, report_high]);
    for (address, packet_size, interval) in endpoints {
        let [packet_low, packet_high] = packet_size.to_le_bytes();
        config.extend_from_slice(&[7, 5, *address, 3, packet_low, packet_high, *interval]);
    }
}
