use std::error::Error;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Parser;
use hidshift::checksum::{crc16_ccitt_false, crc32_ieee};
use hidshift::e2e::{E2eCommand, E2ePacket};
use hidshift::e2e_mirror::{
    MIRROR_E2E_PAYLOAD_MAX, MirrorE2ePacket, OPCODE_HELLO, OPCODE_INJECT_ENDPOINT_IN,
    OPCODE_REGISTER_BEGIN, OPCODE_REGISTER_CHUNK, OPCODE_REGISTER_COMMIT,
    raw_injection_transfer_id,
};
use hidshift::management::{
    ManagementCommand, ManagementOutputTarget, ManagementResult,
};
use hidshift::fallback::{FALLBACK_USB_PRODUCT_ID, FALLBACK_USB_VENDOR_ID};
use hidshift::mirror::validate_mirror_image;
use hidshift::output_target::MirrorCandidateId;
use hidshift_client::{
    ManagementClient, SerialResponseDecoder, encode_serial_request,
};
use serialport::SerialPort;

#[derive(Debug, Parser)]
struct Arguments {
    #[arg(long)]
    host_port: PathBuf,
    #[arg(long)]
    device_flash_port: Option<PathBuf>,
    #[arg(long)]
    skip_flash: bool,
    /// Skip T15 when the Linux hidraw node is not accessible to this user.
    #[arg(long)]
    skip_hidraw: bool,
    #[arg(long, default_value = "e2e/fixtures/mirror/composite-a.hsmi")]
    profile_a: PathBuf,
    #[arg(long, default_value = "e2e/fixtures/mirror/mouse-b.hsmi")]
    profile_b: PathBuf,
    #[arg(long, default_value = "e2e/fixtures/mirror/invalid-duplicate-endpoint.hsmi")]
    invalid_profile: PathBuf,
    #[arg(long, default_value_t = 10)]
    usb_timeout_seconds: u64,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("mirror-e2e: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse();
    if !arguments.skip_flash {
        return Err("automatic flashing is not implemented yet; pass --skip-flash".into());
    }
    let _ = &arguments.device_flash_port;
    let profile_a = fs::read(&arguments.profile_a)?;
    let plan_a = validate_mirror_image(&profile_a)
        .map_err(|reason| format!("Profile A validation failed: {reason:?}"))?;
    let (vid_a, pid_a) = plan_identity(&plan_a.device_descriptor);
    let profile_b = fs::read(&arguments.profile_b)?;
    let plan_b = validate_mirror_image(&profile_b)
        .map_err(|reason| format!("Profile B validation failed: {reason:?}"))?;
    let (vid_b, pid_b) = plan_identity(&plan_b.device_descriptor);
    let invalid_profile = fs::read(&arguments.invalid_profile)?;

    let mut serial = serialport::new(arguments.host_port.to_string_lossy(), 115_200)
        .timeout(Duration::from_millis(100))
        .open()?;
    send_mirror(
        &mut *serial,
        MirrorE2ePacket::new(OPCODE_HELLO, 1, 0, 0, &[])
            .map_err(|error| format!("HELLO packet: {error:?}"))?,
    )?;
    wait_for_text(&mut *serial, b"@HIDSHIFT-MIRROR:READY,1,1", Duration::from_secs(3))?;

    let mut sequence = 2;
    let mut client = ManagementClient::new(80);
    send_management_command(
        &mut *serial,
        &mut client,
        ManagementCommand::ClearMirrorTarget,
        "CLEAR_MIRROR_TARGET",
    )?;
    send_management_command(
        &mut *serial,
        &mut client,
        ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Wired),
        "SELECT_OUTPUT_TARGET(Wired)",
    )?;
    wait_for_usb_identity(
        &mut *serial,
        FALLBACK_USB_VENDOR_ID,
        FALLBACK_USB_PRODUCT_ID,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    println!("T02 passed: HIDShift Wired fallback enumerated");

    let mut fallback_events = open_input_events(
        FALLBACK_USB_VENDOR_ID,
        FALLBACK_USB_PRODUCT_ID,
        3,
        Duration::from_secs(3),
    )?;
    send_normalized(
        &mut *serial,
        E2ePacket {
            sequence: 1_000,
            command: E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0x04, 0, 0, 0, 0, 0],
            },
        },
    )?;
    wait_for_key_event(&mut fallback_events, 30, 1, Duration::from_secs(3))?;
    send_normalized(
        &mut *serial,
        E2ePacket {
            sequence: 1_001,
            command: E2eCommand::ReleaseAll,
        },
    )?;
    wait_for_key_event(&mut fallback_events, 30, 0, Duration::from_secs(3))?;
    println!("T03 passed: normalized keyboard injection reached fallback evdev");

    for (sequence, command, event_type, code, value) in [
        (
            1_002,
            E2eCommand::Mouse {
                buttons: 0,
                x: 12,
                y: 0,
                wheel: 0,
                pan: 0,
            },
            2,
            0,
            12,
        ),
        (
            1_003,
            E2eCommand::Mouse {
                buttons: 0,
                x: 0,
                y: -7,
                wheel: 0,
                pan: 0,
            },
            2,
            1,
            -7,
        ),
        (
            1_004,
            E2eCommand::Mouse {
                buttons: 0,
                x: 0,
                y: 0,
                wheel: 1,
                pan: 0,
            },
            2,
            8,
            1,
        ),
        (
            1_005,
            E2eCommand::Mouse {
                buttons: 0,
                x: 0,
                y: 0,
                wheel: 0,
                pan: 1,
            },
            2,
            6,
            1,
        ),
        (
            1_006,
            E2eCommand::Mouse {
                buttons: 1,
                x: 0,
                y: 0,
                wheel: 0,
                pan: 0,
            },
            1,
            0x110,
            1,
        ),
    ] {
        send_normalized(&mut *serial, E2ePacket { sequence, command })?;
        wait_for_input_event(
            &mut fallback_events,
            event_type,
            code,
            value,
            Duration::from_secs(3),
        )?;
    }
    send_normalized(
        &mut *serial,
        E2ePacket {
            sequence: 1_007,
            command: E2eCommand::ReleaseAll,
        },
    )?;
    wait_for_key_event(&mut fallback_events, 0x110, 0, Duration::from_secs(3))?;
    println!("T04 passed: fallback mouse X/Y/wheel/pan/button reached evdev");

    send_normalized(
        &mut *serial,
        E2ePacket {
            sequence: 1_008,
            command: E2eCommand::Consumer { usage: 0x00e9 },
        },
    )?;
    wait_for_key_event(&mut fallback_events, 115, 1, Duration::from_secs(3))?;
    send_normalized(
        &mut *serial,
        E2ePacket {
            sequence: 1_009,
            command: E2eCommand::ReleaseAll,
        },
    )?;
    wait_for_key_event(&mut fallback_events, 115, 0, Duration::from_secs(3))?;
    println!("T05 passed: fallback Consumer Volume Up reached evdev");

    register_profile(&mut *serial, &profile_a, 1, &mut sequence, true)?;
    wait_for_text(
        &mut *serial,
        b"profile result transfer=1",
        Duration::from_secs(10),
    )?;

    activate_candidate_zero(&mut *serial, &mut client)?;
    wait_for_usb_identity(
        &mut *serial,
        vid_a,
        pid_a,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    println!("T10-T12 passed: registered and enumerated {vid_a:04x}:{pid_a:04x}");

    let mut keyboard_events = open_input_events(vid_a, pid_a, 1, Duration::from_secs(3))?;
    inject_endpoint(
        &mut *serial,
        &mut sequence,
        0x81,
        &[0, 0, 0x04, 0, 0, 0, 0, 0],
    )?;
    wait_for_key_event(&mut keyboard_events, 30, 1, Duration::from_secs(3))?;
    inject_endpoint(&mut *serial, &mut sequence, 0x81, &[0; 8])?;
    wait_for_key_event(&mut keyboard_events, 30, 0, Duration::from_secs(3))?;
    println!("T13 passed: raw endpoint 0x81 produced KEY_A press/release in evdev");

    // Force an edge even when Caps Lock was already enabled by an earlier run.
    set_keyboard_led(&mut keyboard_events, 1, false)?;
    set_keyboard_led(&mut keyboard_events, 1, true)?;
    wait_for_text(
        &mut *serial,
        b"@HIDSHIFT-MIRROR:RAW_OUT,01,1,[02]",
        Duration::from_secs(3),
    )?;
    println!("T14 passed: Caps Lock LED reached raw endpoint 0x01 unchanged");

    if arguments.skip_hidraw {
        println!("T15 skipped explicitly: Linux hidraw access was not requested");
    } else {
        let mut vendor_hidraw = open_hidraw(vid_a, pid_a, 1, Duration::from_secs(3))?;
        let vendor_report = core::array::from_fn::<_, 64, _>(|index| {
            if index == 0 { 0x10 } else { index as u8 }
        });
        inject_endpoint(&mut *serial, &mut sequence, 0x82, &vendor_report)?;
        wait_for_hidraw_report(&mut vendor_hidraw, &vendor_report, Duration::from_secs(3))?;
        vendor_hidraw.write_all(&vendor_report)?;
        let expected_crc = format!(
            "@HIDSHIFT-MIRROR:RAW_OUT_CRC,02,64,{:04X}",
            crc16_ccitt_false(&vendor_report)
        );
        wait_for_text(
            &mut *serial,
            expected_crc.as_bytes(),
            Duration::from_secs(3),
        )?;
        println!("T15 passed: 64-byte Vendor IN/OUT report preserved through hidraw");
    }

    register_profile(&mut *serial, &profile_b, 2, &mut sequence, true)?;
    wait_for_text(
        &mut *serial,
        b"profile result transfer=2",
        Duration::from_secs(10),
    )?;
    activate_candidate_zero(&mut *serial, &mut client)?;
    wait_for_usb_identity(
        &mut *serial,
        vid_b,
        pid_b,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    if usb_identity_present(vid_a, pid_a)? {
        return Err("Profile A remained enumerated after activating Profile B".into());
    }
    println!("T18 passed: switched without reflashing to {vid_b:04x}:{pid_b:04x}");

    register_profile(&mut *serial, &invalid_profile, 3, &mut sequence, false)?;
    if !usb_identity_present(vid_b, pid_b)? {
        return Err("invalid Profile replaced the active presentation".into());
    }
    println!("T19 passed: invalid Profile rejected and Profile B preserved");
    Ok(())
}

fn inject_endpoint(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
    endpoint_address: u8,
    data: &[u8],
) -> Result<(), Box<dyn Error>> {
    let total_length = u8::try_from(data.len()).map_err(|_| "raw report exceeds 255 bytes")?;
    let transfer_id = raw_injection_transfer_id(endpoint_address, total_length);
    let mut offset = 0;
    let mut final_sequence = 0;
    for chunk in data.chunks(MIRROR_E2E_PAYLOAD_MAX) {
        final_sequence = *sequence;
        send_mirror(
            serial,
            MirrorE2ePacket::new(
                OPCODE_INJECT_ENDPOINT_IN,
                final_sequence,
                transfer_id,
                offset as u32,
                chunk,
            )
            .map_err(|error| format!("INJECT_ENDPOINT_IN packet: {error:?}"))?,
        )?;
        *sequence = sequence.wrapping_add(1);
        offset += chunk.len();
    }
    let expected = format!("@HIDSHIFT-MIRROR:INJECTED,{final_sequence}");
    wait_for_text(serial, expected.as_bytes(), Duration::from_secs(3))
}

fn open_input_events(
    vid: u16,
    pid: u16,
    minimum_nodes: usize,
    timeout: Duration,
) -> Result<Vec<fs::File>, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut files = Vec::new();
        for entry in fs::read_dir("/sys/class/input")? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("event")
                || read_hex(path.join("device/id/vendor")) != Some(vid)
                || read_hex(path.join("device/id/product")) != Some(pid)
            {
                continue;
            }
            if let Ok(file) = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(Path::new("/dev/input").join(name))
            {
                files.push(file);
            }
        }
        if files.len() >= minimum_nodes {
            return Ok(files);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!(
        "fewer than {minimum_nodes} evdev nodes found for {vid:04x}:{pid:04x}"
    )
    .into())
}

fn set_keyboard_led(
    files: &mut [fs::File],
    led_code: u16,
    enabled: bool,
) -> Result<(), Box<dyn Error>> {
    const INPUT_EVENT_LEN: usize = 24;
    const EV_SYN: u16 = 0;
    const EV_LED: u16 = 0x11;
    let mut events = [0u8; INPUT_EVENT_LEN * 2];
    events[16..18].copy_from_slice(&EV_LED.to_ne_bytes());
    events[18..20].copy_from_slice(&led_code.to_ne_bytes());
    events[20..24].copy_from_slice(&i32::from(enabled).to_ne_bytes());
    events[INPUT_EVENT_LEN + 16..INPUT_EVENT_LEN + 18].copy_from_slice(&EV_SYN.to_ne_bytes());
    let mut last_error = None;
    for file in files {
        match file.write_all(&events) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error
        .map_or_else(|| "no keyboard evdev node".into(), |error| error.into()))
}

fn open_hidraw(
    vid: u16,
    pid: u16,
    interface_number: u8,
    timeout: Duration,
) -> Result<fs::File, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut inaccessible = None;
    while Instant::now() < deadline {
        for entry in fs::read_dir("/sys/class/hidraw")? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Ok(device_path) = fs::canonicalize(path.join("device")) else {
                continue;
            };
            if ancestor_hex(&device_path, "idVendor") != Some(vid)
                || ancestor_hex(&device_path, "idProduct") != Some(pid)
                || ancestor_hex(&device_path, "bInterfaceNumber")
                    != Some(u16::from(interface_number))
            {
                continue;
            }
            match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(Path::new("/dev").join(name))
            {
                Ok(file) => return Ok(file),
                Err(error) => inaccessible = Some((name.to_owned(), error)),
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if let Some((name, error)) = inaccessible {
        return Err(format!(
            "/dev/{name} matches {vid:04x}:{pid:04x} interface {interface_number} but cannot be opened read/write ({error}); configure hidraw udev permissions or pass --skip-hidraw"
        )
        .into());
    }
    Err(format!(
        "no hidraw node found for {vid:04x}:{pid:04x} interface {interface_number}"
    )
    .into())
}

fn ancestor_hex(path: &Path, file_name: &str) -> Option<u16> {
    path.ancestors()
        .find_map(|ancestor| read_hex(ancestor.join(file_name)))
}

fn wait_for_hidraw_report(
    file: &mut fs::File,
    expected: &[u8],
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut report = [0; 64];
    while Instant::now() < deadline {
        match file.read(&mut report) {
            Ok(length) if &report[..length] == expected => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error.into()),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err("timeout waiting for expected hidraw report".into())
}

fn wait_for_key_event(
    files: &mut [fs::File],
    key_code: u16,
    value: i32,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    wait_for_input_event(files, 1, key_code, value, timeout)
}

fn wait_for_input_event(
    files: &mut [fs::File],
    event_type: u16,
    event_code: u16,
    value: i32,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    const INPUT_EVENT_LEN: usize = 24;
    let deadline = Instant::now() + timeout;
    let mut bytes = [0; INPUT_EVENT_LEN * 8];
    let mut observed = Vec::new();
    while Instant::now() < deadline {
        for file in files.iter_mut() {
            match file.read(&mut bytes) {
                Ok(length) => {
                    for event in bytes[..length].chunks_exact(INPUT_EVENT_LEN) {
                        let observed_type = u16::from_ne_bytes([event[16], event[17]]);
                        let code = u16::from_ne_bytes([event[18], event[19]]);
                        let event_value =
                            i32::from_ne_bytes([event[20], event[21], event[22], event[23]]);
                        if observed_type != 0 {
                            if observed.len() == 16 {
                                observed.remove(0);
                            }
                            observed.push((observed_type, code, event_value));
                        }
                        if observed_type == event_type && code == event_code && event_value == value {
                            return Ok(());
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(format!(
        "timeout waiting for evdev type {event_type} code {event_code} value {value}; observed {observed:?}"
    )
    .into())
}

fn register_profile(
    serial: &mut dyn SerialPort,
    image: &[u8],
    transfer_id: u32,
    sequence: &mut u32,
    expect_accept: bool,
) -> Result<(), Box<dyn Error>> {
    let profile_hash = nonzero_hash(crc32_ieee(image));
    let mut begin_payload = [0; 8];
    begin_payload[..4].copy_from_slice(&crc32_ieee(image).to_le_bytes());
    begin_payload[4..].copy_from_slice(&profile_hash.to_le_bytes());
    send_mirror(
        serial,
        MirrorE2ePacket::new(
            OPCODE_REGISTER_BEGIN,
            *sequence,
            transfer_id,
            image.len() as u32,
            &begin_payload,
        )
        .map_err(|error| format!("REGISTER_BEGIN packet: {error:?}"))?,
    )?;
    *sequence = sequence.wrapping_add(1);
    wait_for_text(serial, b"@HIDSHIFT-MIRROR:BEGIN", Duration::from_secs(3))?;
    for (index, chunk) in image.chunks(47).enumerate() {
        let offset = (index * 47) as u32;
        send_mirror(
            serial,
            MirrorE2ePacket::new(
                OPCODE_REGISTER_CHUNK,
                *sequence,
                transfer_id,
                offset,
                chunk,
            )
                .map_err(|error| format!("REGISTER_CHUNK packet: {error:?}"))?,
        )?;
        *sequence = sequence.wrapping_add(1);
    }
    send_mirror(
        serial,
        MirrorE2ePacket::new(OPCODE_REGISTER_COMMIT, *sequence, transfer_id, 0, &[])
            .map_err(|error| format!("REGISTER_COMMIT packet: {error:?}"))?,
    )?;
    *sequence = sequence.wrapping_add(1);
    let expected = if expect_accept {
        b"@HIDSHIFT-MIRROR:REGISTERED".as_slice()
    } else {
        // ProfileResultStatus::InvalidImage and
        // MirrorRejectReason::DuplicateEndpointAddress.
        b"commit,2,8".as_slice()
    };
    wait_for_text(serial, expected, Duration::from_secs(10))?;
    Ok(())
}

fn activate_candidate_zero(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
) -> Result<(), Box<dyn Error>> {
    send_management_command(
        serial,
        client,
        ManagementCommand::SetMirrorTarget(MirrorCandidateId(0)),
        "SET_MIRROR_TARGET",
    )?;
    send_management_command(
        serial,
        client,
        ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Wired),
        "SELECT_OUTPUT_TARGET(Wired)",
    )?;
    Ok(())
}

fn send_management_command(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
    command: ManagementCommand,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    let pending = client
        .begin(command)
        .map_err(|error| format!("{name} request: {error:?}"))?;
    serial.write_all(&encode_serial_request(pending))?;
    let response = wait_management_response(serial, client, Duration::from_secs(5))?;
    if response.result != ManagementResult::Ok {
        return Err(format!("{name} failed: {:?}", response.result).into());
    }
    Ok(())
}

fn send_mirror(
    serial: &mut dyn SerialPort,
    packet: MirrorE2ePacket,
) -> Result<(), Box<dyn Error>> {
    serial.write_all(&packet.encode_line())?;
    serial.write_all(b"\n")?;
    Ok(())
}

fn send_normalized(
    serial: &mut dyn SerialPort,
    packet: E2ePacket,
) -> Result<(), Box<dyn Error>> {
    serial.write_all(&packet.encode_line())?;
    serial.write_all(b"\n")?;
    Ok(())
}

fn wait_for_text(
    serial: &mut dyn SerialPort,
    expected: &[u8],
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    let mut byte = [0; 1];
    while Instant::now() < deadline {
        match serial.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' || byte[0] == b'\r' => {
                if !line.is_empty() {
                    eprintln!("host: {}", String::from_utf8_lossy(&line));
                }
                if line.windows(expected.len()).any(|window| window == expected) {
                    return Ok(());
                }
                line.clear();
            }
            Ok(1) if line.len() < 512 => line.push(byte[0]),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err(format!("timeout waiting for {}", String::from_utf8_lossy(expected)).into())
}

fn wait_management_response(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
    timeout: Duration,
) -> Result<hidshift::ManagementResponse, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut decoder = SerialResponseDecoder::default();
    let mut bytes = [0; 128];
    while Instant::now() < deadline {
        match serial.read(&mut bytes) {
            Ok(length) => {
                eprint!("{}", String::from_utf8_lossy(&bytes[..length]));
                for response in decoder.push(&bytes[..length]) {
                    if let Some(response) = client
                        .accept_notification(&response)
                        .map_err(|error| format!("management response: {error:?}"))?
                    {
                        return Ok(response);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("management response timeout".into())
}

fn wait_for_usb_identity(
    serial: &mut dyn SerialPort,
    vid: u16,
    pid: u16,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    let mut uart = [0; 256];
    while Instant::now() < deadline {
        match serial.read(&mut uart) {
            Ok(length) => eprint!("{}", String::from_utf8_lossy(&uart[..length])),
            Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {}
            Err(error) => return Err(error.into()),
        }
        for entry in fs::read_dir("/sys/bus/usb/devices")? {
            let path = entry?.path();
            if read_hex(path.join("idVendor")) == Some(vid)
                && read_hex(path.join("idProduct")) == Some(pid)
            {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("USB identity {vid:04x}:{pid:04x} did not enumerate").into())
}

fn read_hex(path: impl AsRef<Path>) -> Option<u16> {
    u16::from_str_radix(fs::read_to_string(path).ok()?.trim(), 16).ok()
}

fn usb_identity_present(vid: u16, pid: u16) -> Result<bool, Box<dyn Error>> {
    for entry in fs::read_dir("/sys/bus/usb/devices")? {
        let path = entry?.path();
        if read_hex(path.join("idVendor")) == Some(vid)
            && read_hex(path.join("idProduct")) == Some(pid)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn plan_identity(device_descriptor: &[u8; 18]) -> (u16, u16) {
    (
        u16::from_le_bytes([device_descriptor[8], device_descriptor[9]]),
        u16::from_le_bytes([device_descriptor[10], device_descriptor[11]]),
    )
}

const fn nonzero_hash(hash: u32) -> u32 {
    if hash == 0 { 1 } else { hash }
}
