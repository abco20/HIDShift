use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::Parser;
use hidshift::checksum::{crc32_ieee};
use hidshift::e2e_mirror::{
    MirrorE2ePacket, OPCODE_HELLO, OPCODE_REGISTER_BEGIN, OPCODE_REGISTER_CHUNK,
    OPCODE_REGISTER_COMMIT,
};
use hidshift::management::{
    ManagementCommand, ManagementOutputTarget, ManagementResult,
};
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
    register_profile(&mut *serial, &profile_a, 1, &mut sequence, true)?;
    wait_for_text(
        &mut *serial,
        b"profile result transfer=1",
        Duration::from_secs(10),
    )?;

    let mut client = ManagementClient::new(80);
    activate_candidate_zero(&mut *serial, &mut client)?;
    wait_for_usb_identity(
        &mut *serial,
        vid_a,
        pid_a,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    println!("T10-T12 passed: registered and enumerated {vid_a:04x}:{pid_a:04x}");

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
