use std::error::Error;
use std::fs;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Parser;
use hidshift::checksum::{crc16_ccitt_false, crc32_ieee};
use hidshift::e2e::{E2eCommand, E2ePacket};
use hidshift::e2e_mirror::{
    MIRROR_E2E_PAYLOAD_MAX, MirrorE2ePacket, OPCODE_DROP_SPI_CELLS, OPCODE_HELLO,
    OPCODE_INJECT_ENDPOINT_IN, OPCODE_INJECT_SPI_CRC_FAILURE, OPCODE_READ_MOCK_STATUS,
    OPCODE_REGISTER_BEGIN, OPCODE_REGISTER_CHUNK, OPCODE_REGISTER_COMMIT, OPCODE_RESET_MOCK_STATUS,
    OPCODE_SET_CONTROL_RESPONSE, raw_injection_transfer_id,
};
use hidshift::fallback::{FALLBACK_USB_PRODUCT_ID, FALLBACK_USB_VENDOR_ID};
use hidshift::ids::HostId;
use hidshift::management::{
    ManagementCommand, ManagementOutputTarget, ManagementResponsePayload, ManagementResult,
};
use hidshift::mirror::{UsbDevicePlan, validate_mirror_image};
use hidshift::output_target::{MirrorCandidateId, OutputTargetAvailability};
use hidshift_client::{ManagementClient, SerialResponseDecoder, encode_serial_request};
use serialport::SerialPort;

#[derive(Debug, Parser)]
struct Arguments {
    #[arg(long)]
    host_port: PathBuf,
    #[arg(long)]
    device_flash_port: Option<PathBuf>,
    #[arg(long)]
    skip_flash: bool,
    /// Run only the SPI link-loss/no-failover scenario against existing images.
    #[arg(long)]
    spi_loss_only: bool,
    /// Skip T15 when the Linux hidraw node is not accessible to this user.
    #[arg(long)]
    skip_hidraw: bool,
    /// Run BLE Management and release/suppression E2E through this bonded peer.
    #[arg(long)]
    ble_address: Option<String>,
    /// Re-pair the BLE peer even when reusing already flashed images.
    #[arg(long)]
    pair_ble: bool,
    /// Linux Bluetooth controller accepted by hardware-E2E Host firmware.
    #[arg(long)]
    linux_controller_address: Option<String>,
    #[arg(long, default_value_t = 2)]
    ble_host_slot: u8,
    #[arg(long, default_value = "tools/hidshiftctl/target/release/hidshiftctl")]
    hidshiftctl: PathBuf,
    #[arg(long, default_value = "e2e/fixtures/mirror/composite-a.hsmi")]
    profile_a: PathBuf,
    #[arg(long, default_value = "e2e/fixtures/mirror/mouse-b.hsmi")]
    profile_b: PathBuf,
    #[arg(
        long,
        default_value = "e2e/fixtures/mirror/invalid-duplicate-endpoint.hsmi"
    )]
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
        let device_port = arguments
            .device_flash_port
            .as_deref()
            .ok_or("--device-flash-port is required unless --skip-flash is used")?;
        if arguments.ble_address.is_some() && arguments.linux_controller_address.is_none() {
            return Err(
                "--linux-controller-address is required when flashing for --ble-address E2E".into(),
            );
        }
        flash_firmware(
            &arguments.host_port,
            device_port,
            arguments.linux_controller_address.as_deref(),
        )?;
    }
    let mut serial = serialport::new(arguments.host_port.to_string_lossy(), 115_200)
        .timeout(Duration::from_millis(100))
        .open()?;
    wait_for_mirror_ready(&mut *serial)?;

    let mut sequence = 2;
    let mut client = ManagementClient::new(80);
    wait_for_management_ready(&mut *serial, &mut client, Duration::from_secs(30))?;
    if (!arguments.skip_flash || arguments.pair_ble)
        && let Some(address) = arguments.ble_address.as_deref()
    {
        pair_linux_ble_peer(&mut *serial, &mut client, address, arguments.ble_host_slot)?;
    }
    if arguments.spi_loss_only {
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
        wait_for_wired_ready(
            &mut *serial,
            &mut client,
            Duration::from_secs(arguments.usb_timeout_seconds),
        )?;
        let device_port = arguments
            .device_flash_port
            .as_deref()
            .ok_or("--device-flash-port is required with --spi-loss-only to restore the link")?;
        verify_spi_loss(
            &mut *serial,
            &mut sequence,
            &mut client,
            device_port,
            arguments.usb_timeout_seconds,
        )?;
        return Ok(());
    }

    let profile_a = fs::read(&arguments.profile_a)?;
    let plan_a = validate_mirror_image(&profile_a)
        .map_err(|reason| format!("Profile A validation failed: {reason:?}"))?;
    let (vid_a, pid_a) = plan_identity(&plan_a.device_descriptor);
    let profile_b = fs::read(&arguments.profile_b)?;
    let plan_b = validate_mirror_image(&profile_b)
        .map_err(|reason| format!("Profile B validation failed: {reason:?}"))?;
    let (vid_b, pid_b) = plan_identity(&plan_b.device_descriptor);
    let invalid_profile = fs::read(&arguments.invalid_profile)?;

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
    wait_for_wired_ready(
        &mut *serial,
        &mut client,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    println!("T02 passed: HIDShift Wired fallback enumerated");

    let mut fallback_events = open_input_events(
        FALLBACK_USB_VENDOR_ID,
        FALLBACK_USB_PRODUCT_ID,
        3,
        Duration::from_secs(3),
    )?;
    // A previous interrupted E2E run may leave the injected physical state
    // pressed. Reset that source boundary before asserting a fresh edge so
    // suppression state cannot make a rerun order-dependent.
    send_normalized(
        &mut *serial,
        E2ePacket {
            sequence: 999,
            command: E2eCommand::ReleaseAll,
        },
    )?;
    std::thread::sleep(Duration::from_millis(50));
    drain_input_events(&mut fallback_events)?;
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

    set_keyboard_led(&mut fallback_events, 1, false)?;
    set_keyboard_led(&mut fallback_events, 1, true)?;
    wait_for_text(
        &mut *serial,
        b"@HIDSHIFT-MIRROR:WIRED_LEDS,02",
        Duration::from_secs(3),
    )?;
    println!("T23 passed: fallback Caps Lock output crossed SPI to Host S3");

    if let Some(address) = arguments.ble_address.as_deref() {
        verify_ble_management_switching(
            &mut *serial,
            &arguments.hidshiftctl,
            address,
            arguments.ble_host_slot,
            &mut fallback_events,
            arguments.usb_timeout_seconds,
        )?;
    } else {
        println!("T06-T08 skipped: pass --ble-address for bonded BlueZ Management E2E");
    }

    if let Some(device_port) = arguments.device_flash_port.as_deref() {
        verify_spi_loss(
            &mut *serial,
            &mut sequence,
            &mut client,
            device_port,
            arguments.usb_timeout_seconds,
        )?;
    } else {
        println!("T26 skipped: --device-flash-port is required to restore the SPI link");
    }

    register_profile(&mut *serial, &profile_a, 1, &mut sequence, true)?;
    wait_for_text(
        &mut *serial,
        b"profile result transfer=1",
        Duration::from_secs(10),
    )?;
    wait_for_mirror_candidate(
        &mut *serial,
        &mut client,
        nonzero_hash(crc32_ieee(&profile_a)),
        Duration::from_secs(5),
    )?;

    activate_candidate_zero(&mut *serial, &mut client)?;
    wait_for_usb_identity(
        &mut *serial,
        vid_a,
        pid_a,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    wait_for_wired_ready(
        &mut *serial,
        &mut client,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    verify_usb_plan(&plan_a)?;
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

    reset_mock_status(&mut *serial, &mut sequence)?;
    arm_spi_crc_failure(&mut *serial, &mut sequence)?;
    inject_endpoint(
        &mut *serial,
        &mut sequence,
        0x81,
        &[0, 0, 0x05, 0, 0, 0, 0, 0],
    )?;
    wait_for_key_event(&mut keyboard_events, 48, 1, Duration::from_secs(3))?;
    inject_endpoint(&mut *serial, &mut sequence, 0x81, &[0; 8])?;
    wait_for_key_event(&mut keyboard_events, 48, 0, Duration::from_secs(3))?;
    assert_spi_crc_retry(&mut *serial, &mut sequence)?;
    println!("T25 passed: corrupted SPI cell was retried and delivered once");

    // Force an edge even when Caps Lock was already enabled by an earlier run.
    set_keyboard_led(&mut keyboard_events, 1, false)?;
    set_keyboard_led(&mut keyboard_events, 1, true)?;
    wait_for_text(
        &mut *serial,
        b"@HIDSHIFT-MIRROR:RAW_OUT,01,1,[02]",
        Duration::from_secs(3),
    )?;
    println!("T14 passed: Caps Lock LED reached raw endpoint 0x01 unchanged");

    if let Some(address) = arguments.ble_address.as_deref() {
        let host_slot = arguments.ble_host_slot.to_string();
        run_ble_management(
            &arguments.hidshiftctl,
            address,
            &["target", "ble", &host_slot],
        )?;
        run_ble_management(&arguments.hidshiftctl, address, &["mirror", "status"])?;
    } else {
        send_management_command(
            &mut *serial,
            &mut client,
            ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Ble(HostId(1))),
            "SELECT_OUTPUT_TARGET(BLE 1)",
        )?;
    }
    wait_for_usb_identity(
        &mut *serial,
        FALLBACK_USB_VENDOR_ID,
        FALLBACK_USB_PRODUCT_ID,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    println!("T20/T22 passed: BLE selection forced fallback while retaining Mirror A");
    if let Some(address) = arguments.ble_address.as_deref() {
        run_ble_management(&arguments.hidshiftctl, address, &["target", "usb"])?;
    } else {
        send_management_command(
            &mut *serial,
            &mut client,
            ManagementCommand::SelectOutputTarget(ManagementOutputTarget::Wired),
            "SELECT_OUTPUT_TARGET(Wired)",
        )?;
    }
    wait_for_usb_identity(
        &mut *serial,
        vid_a,
        pid_a,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    wait_for_wired_ready(
        &mut *serial,
        &mut client,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    println!("T21 passed: Wired selection restored saved Mirror A");

    if arguments.skip_hidraw {
        println!("T15 skipped explicitly: Linux hidraw access was not requested");
    } else {
        let mut vendor_hidraw = open_hidraw(vid_a, pid_a, 1, Duration::from_secs(3))?;
        let vendor_report =
            core::array::from_fn::<_, 64, _>(|index| if index == 0 { 0x10 } else { index as u8 });
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

        let expected_feature =
            core::array::from_fn::<_, 17, _>(
                |index| {
                    if index == 0 { 0x10 } else { 0xa0 + index as u8 }
                },
            );
        set_control_response(&mut *serial, &mut sequence, 0, &expected_feature)?;
        let mut feature = [0u8; 17];
        feature[0] = 0x10;
        let length = hidraw_get_feature(&vendor_hidraw, &mut feature)?;
        if length != feature.len() || feature != expected_feature {
            return Err(format!(
                "GET_REPORT mismatch: length={length} actual={feature:02x?} expected={expected_feature:02x?}"
            )
            .into());
        }
        wait_for_text(
            &mut *serial,
            b"[A1, 01, 10, 03, 01, 00, 11, 00],0",
            Duration::from_secs(3),
        )?;
        println!("T16 passed: HIDIOCGFEATURE crossed deferred EP0 and preserved Report ID");

        set_control_response(&mut *serial, &mut sequence, 0, &[])?;
        let feature_set =
            core::array::from_fn::<_, 17, _>(
                |index| {
                    if index == 0 { 0x10 } else { 0x50 + index as u8 }
                },
            );
        let length = hidraw_set_feature(&vendor_hidraw, &feature_set)?;
        if length != feature_set.len() {
            return Err(format!(
                "SET_REPORT length mismatch: {length} != {}",
                feature_set.len()
            )
            .into());
        }
        let expected_request = format!(
            "[21, 09, 10, 03, 01, 00, 11, 00],17,{:04X}",
            crc16_ccitt_false(&feature_set)
        );
        wait_for_text(
            &mut *serial,
            expected_request.as_bytes(),
            Duration::from_secs(3),
        )?;
        println!("T17 passed: HIDIOCSFEATURE payload and Report ID reached synthetic Host");
    }

    register_profile(&mut *serial, &profile_b, 2, &mut sequence, true)?;
    wait_for_text(
        &mut *serial,
        b"profile result transfer=2",
        Duration::from_secs(10),
    )?;
    wait_for_mirror_candidate(
        &mut *serial,
        &mut client,
        nonzero_hash(crc32_ieee(&profile_b)),
        Duration::from_secs(5),
    )?;
    activate_candidate_zero(&mut *serial, &mut client)?;
    wait_for_usb_identity(
        &mut *serial,
        vid_b,
        pid_b,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    wait_for_wired_ready(
        &mut *serial,
        &mut client,
        Duration::from_secs(arguments.usb_timeout_seconds),
    )?;
    if usb_identity_present(vid_a, pid_a)? {
        return Err("Profile A remained enumerated after activating Profile B".into());
    }
    verify_usb_plan(&plan_b)?;
    println!("T18 passed: switched without reflashing to {vid_b:04x}:{pid_b:04x}");

    register_profile(&mut *serial, &invalid_profile, 3, &mut sequence, false)?;
    if !usb_identity_present(vid_b, pid_b)? {
        return Err("invalid Profile replaced the active presentation".into());
    }
    println!("T19 passed: invalid Profile rejected and Profile B preserved");

    if let Some(device_port) = &arguments.device_flash_port {
        reset_device_s3(device_port)?;
        wait_for_usb_identity(
            &mut *serial,
            vid_b,
            pid_b,
            Duration::from_secs(arguments.usb_timeout_seconds),
        )?;
        wait_for_wired_ready(
            &mut *serial,
            &mut client,
            Duration::from_secs(arguments.usb_timeout_seconds),
        )?;
        println!("T24 passed: Device S3 reboot restored saved Profile B");
    } else {
        println!("T24 skipped: --device-flash-port was not provided");
    }
    Ok(())
}

fn flash_firmware(
    host_port: &Path,
    device_port: &Path,
    linux_controller_address: Option<&str>,
) -> Result<(), Box<dyn Error>> {
    if linux_controller_address.is_some_and(|address| !valid_bluetooth_address(address)) {
        return Err("invalid --linux-controller-address".into());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("mirror-runner is not below the repository root")?;
    let export_file = root.join(".mise/esp/export-esp.sh");
    if !export_file.is_file() {
        return Err("ESP toolchain is not installed; run `mise run esp:install`".into());
    }

    let controller = linux_controller_address.unwrap_or("");
    let build_script = format!(
        "source '{}' && \
         export HIDSHIFT_E2E_LINUX_ADDRESS='{}' && \
         cargo +esp build --locked -Zbuild-std=core,alloc --release \
           --manifest-path firmware/Cargo.toml --bin firmware \
           --features hardware-e2e,dual-s3-wired \
           --target xtensa-esp32s3-none-elf && \
         cargo +esp build --locked -Zbuild-std=core --release \
           --manifest-path device-firmware/Cargo.toml --bin hidshift-device \
           --features hardware-e2e \
           --target xtensa-esp32s3-none-elf",
        export_file.display(),
        controller
    );
    run_command(
        Command::new("bash")
            .args(["-lc", &build_script])
            .current_dir(root),
        "build Host and Device S3 firmware",
    )?;
    run_command(
        Command::new("espflash")
            .args(["erase-region", "--chip", "esp32s3", "--port"])
            .arg(device_port)
            .args(["0x194000", "0x10000"])
            .current_dir(root),
        "erase Device S3 Mirror profile partition",
    )?;
    run_command(
        Command::new("espflash")
            .args(["flash", "--chip", "esp32s3", "--port"])
            .arg(device_port)
            .args([
                "--partition-table",
                "partitions/bridge.csv",
                "--target-app-partition",
                "factory",
                "device-firmware/target/xtensa-esp32s3-none-elf/release/hidshift-device",
            ])
            .current_dir(root),
        "flash Device S3",
    )?;
    run_command(
        Command::new("espflash")
            .args(["erase-region", "--chip", "esp32s3", "--port"])
            .arg(host_port)
            .args(["0x190000", "0x4000"])
            .current_dir(root),
        "erase Host S3 settings partition",
    )?;
    run_command(
        Command::new("espflash")
            .args(["flash", "--chip", "esp32s3", "--port"])
            .arg(host_port)
            .args([
                "--partition-table",
                "partitions/bridge.csv",
                "--target-app-partition",
                "factory",
                "target/xtensa-esp32s3-none-elf/release/firmware",
            ])
            .current_dir(root),
        "flash Host S3",
    )?;
    Ok(())
}

fn valid_bluetooth_address(address: &str) -> bool {
    let bytes = address.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn run_command(command: &mut Command, description: &str) -> Result<(), Box<dyn Error>> {
    let status = command.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{description} failed with {status}").into())
    }
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

fn set_control_response(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
    status: u8,
    data: &[u8],
) -> Result<(), Box<dyn Error>> {
    if data.len() + 1 > MIRROR_E2E_PAYLOAD_MAX {
        return Err("synthetic control response exceeds one Mirror E2E packet".into());
    }
    let mut payload = [0u8; MIRROR_E2E_PAYLOAD_MAX];
    payload[0] = status;
    payload[1..1 + data.len()].copy_from_slice(data);
    let sent_sequence = *sequence;
    send_mirror(
        serial,
        MirrorE2ePacket::new(
            OPCODE_SET_CONTROL_RESPONSE,
            sent_sequence,
            0,
            0,
            &payload[..1 + data.len()],
        )
        .map_err(|error| format!("SET_CONTROL_RESPONSE packet: {error:?}"))?,
    )?;
    *sequence = sequence.wrapping_add(1);
    let expected = format!("@HIDSHIFT-MIRROR:CONTROL_RESPONSE_SET,{sent_sequence}");
    wait_for_text(serial, expected.as_bytes(), Duration::from_secs(3))
}

fn reset_mock_status(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
) -> Result<(), Box<dyn Error>> {
    let sent_sequence = *sequence;
    send_mirror(
        serial,
        MirrorE2ePacket::new(OPCODE_RESET_MOCK_STATUS, sent_sequence, 0, 0, &[])
            .map_err(|error| format!("RESET_MOCK_STATUS packet: {error:?}"))?,
    )?;
    *sequence = sequence.wrapping_add(1);
    let expected = format!("@HIDSHIFT-MIRROR:MOCK_STATUS_RESET,{sent_sequence}");
    wait_for_text(serial, expected.as_bytes(), Duration::from_secs(3))
}

fn arm_spi_crc_failure(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
) -> Result<(), Box<dyn Error>> {
    let sent_sequence = *sequence;
    send_mirror(
        serial,
        MirrorE2ePacket::new(OPCODE_INJECT_SPI_CRC_FAILURE, sent_sequence, 1, 0, &[])
            .map_err(|error| format!("INJECT_SPI_CRC_FAILURE packet: {error:?}"))?,
    )?;
    *sequence = sequence.wrapping_add(1);
    let expected = format!("@HIDSHIFT-MIRROR:SPI_CRC_ARMED,{sent_sequence},1");
    wait_for_text(serial, expected.as_bytes(), Duration::from_secs(3))
}

fn arm_spi_cell_drop(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
    cells: u32,
) -> Result<(), Box<dyn Error>> {
    let sent_sequence = *sequence;
    send_mirror(
        serial,
        MirrorE2ePacket::new(OPCODE_DROP_SPI_CELLS, sent_sequence, cells, 0, &[])
            .map_err(|error| format!("DROP_SPI_CELLS packet: {error:?}"))?,
    )?;
    *sequence = sequence.wrapping_add(1);
    let expected = format!("@HIDSHIFT-MIRROR:SPI_DROP_ARMED,{sent_sequence},{cells}");
    wait_for_text(serial, expected.as_bytes(), Duration::from_secs(3))
}

fn verify_spi_loss(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
    client: &mut ManagementClient,
    device_port: &Path,
    usb_timeout_seconds: u64,
) -> Result<(), Box<dyn Error>> {
    arm_spi_cell_drop(serial, sequence, 5_000)?;
    std::thread::sleep(Duration::from_millis(1_650));
    let status = output_target_status(serial, client)?;
    if status.selected != ManagementOutputTarget::Wired
        || status.active.is_some()
        || status.availability != OutputTargetAvailability::Unavailable
    {
        return Err(format!(
            "SPI loss unexpectedly changed/routed the target: selected={:?} active={:?} availability={:?}",
            status.selected, status.active, status.availability
        )
        .into());
    }
    println!("T26 passed: SPI loss kept Wired selected with no active failover target");

    // T26 models an unavailable inter-chip link. Reset the Device S3 after
    // observing that terminal state so the remaining suite starts from a
    // deterministic SPI slave DMA transaction.
    reset_device_s3(device_port)?;
    wait_for_wired_ready(serial, client, Duration::from_secs(10))?;
    wait_for_usb_identity(
        serial,
        FALLBACK_USB_VENDOR_ID,
        FALLBACK_USB_PRODUCT_ID,
        Duration::from_secs(usb_timeout_seconds + 5),
    )?;
    println!("T26 cleanup: Device S3 reset restored Wired fallback");
    Ok(())
}

fn assert_spi_crc_retry(
    serial: &mut dyn SerialPort,
    sequence: &mut u32,
) -> Result<(), Box<dyn Error>> {
    let sent_sequence = *sequence;
    send_mirror(
        serial,
        MirrorE2ePacket::new(OPCODE_READ_MOCK_STATUS, sent_sequence, 0, 0, &[])
            .map_err(|error| format!("READ_MOCK_STATUS packet: {error:?}"))?,
    )?;
    *sequence = sequence.wrapping_add(1);
    let expected = format!("@HIDSHIFT-MIRROR:MOCK_STATUS,{sent_sequence},0,1,1");
    wait_for_text(serial, expected.as_bytes(), Duration::from_secs(3))
}

fn hidraw_get_feature(file: &fs::File, data: &mut [u8]) -> Result<usize, Box<dyn Error>> {
    hidraw_feature_ioctl(file, data.as_mut_ptr(), data.len(), 0x07)
}

fn hidraw_set_feature(file: &fs::File, data: &[u8]) -> Result<usize, Box<dyn Error>> {
    hidraw_feature_ioctl(file, data.as_ptr().cast_mut(), data.len(), 0x06)
}

fn hidraw_feature_ioctl(
    file: &fs::File,
    data: *mut u8,
    length: usize,
    number: u8,
) -> Result<usize, Box<dyn Error>> {
    if length > 0x3fff {
        return Err("hidraw ioctl payload exceeds Linux _IOC size field".into());
    }
    let request = hidraw_feature_request(length, number);
    // SAFETY: `data` points to `length` readable/writable bytes for the
    // duration of the ioctl, and `file` is an open hidraw descriptor.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), request, data) };
    if result < 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(result as usize)
    }
}

const fn hidraw_feature_request(length: usize, number: u8) -> u64 {
    const IOC_READ_WRITE: u64 = 3;
    (IOC_READ_WRITE << 30) | ((length as u64) << 16) | ((b'H' as u64) << 8) | number as u64
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
    Err(format!("fewer than {minimum_nodes} evdev nodes found for {vid:04x}:{pid:04x}").into())
}

fn open_named_input_events(name: &str, timeout: Duration) -> Result<Vec<fs::File>, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let mut files = Vec::new();
        for entry in fs::read_dir("/sys/class/input")? {
            let path = entry?.path();
            let Some(event_name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if !event_name.starts_with("event")
                || fs::read_to_string(path.join("device/name"))
                    .ok()
                    .is_none_or(|value| value.trim() != name)
            {
                continue;
            }
            if let Ok(file) = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(Path::new("/dev/input").join(event_name))
            {
                files.push(file);
            }
        }
        if !files.is_empty() {
            return Ok(files);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("no readable evdev node named {name:?}").into())
}

fn run_ble_management(
    hidshiftctl: &Path,
    address: &str,
    arguments: &[&str],
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_failure = String::new();
    while Instant::now() < deadline {
        let output = Command::new(hidshiftctl)
            .args(["--ble", "--address", address])
            .args(arguments)
            .output()?;
        if output.status.success() {
            return Ok(());
        }
        last_failure = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "BLE Management command {:?} failed after reconnect retries: {last_failure}",
        arguments
    )
    .into())
}

fn verify_ble_management_switching(
    serial: &mut dyn SerialPort,
    hidshiftctl: &Path,
    address: &str,
    host_slot: u8,
    fallback_events: &mut [fs::File],
    usb_timeout_seconds: u64,
) -> Result<(), Box<dyn Error>> {
    if !(1..=4).contains(&host_slot) {
        return Err(format!("--ble-host-slot must be 1..=4, got {host_slot}").into());
    }
    if !hidshiftctl.is_file() {
        return Err(format!(
            "{} does not exist; build hidshiftctl --release first",
            hidshiftctl.display()
        )
        .into());
    }
    drain_input_events(fallback_events)?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_100,
            command: E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0x04, 0, 0, 0, 0, 0],
            },
        },
    )?;
    wait_for_key_event(fallback_events, 30, 1, Duration::from_secs(3))?;

    let host_slot_text = host_slot.to_string();
    run_ble_management(hidshiftctl, address, &["target", "ble", &host_slot_text])?;
    wait_for_key_event(fallback_events, 30, 0, Duration::from_secs(3))?;
    let mut ble_events = open_named_input_events("HIDShift Keyboard", Duration::from_secs(8))?;
    drain_input_events(&mut ble_events)?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_101,
            command: E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0x04, 0, 0, 0, 0, 0],
            },
        },
    )?;
    assert_no_input_event(&mut ble_events, 1, 30, 1, Duration::from_millis(500))?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_102,
            command: E2eCommand::ReleaseAll,
        },
    )?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_103,
            command: E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0x05, 0, 0, 0, 0, 0],
            },
        },
    )?;
    wait_for_key_event(&mut ble_events, 48, 1, Duration::from_secs(3))?;

    run_ble_management(hidshiftctl, address, &["target", "usb"])?;
    wait_for_key_event(&mut ble_events, 48, 0, Duration::from_secs(3))?;
    wait_for_usb_identity(
        serial,
        FALLBACK_USB_VENDOR_ID,
        FALLBACK_USB_PRODUCT_ID,
        Duration::from_secs(usb_timeout_seconds),
    )?;
    drain_input_events(fallback_events)?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_104,
            command: E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0x05, 0, 0, 0, 0, 0],
            },
        },
    )?;
    assert_no_input_event(fallback_events, 1, 48, 1, Duration::from_millis(500))?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_105,
            command: E2eCommand::ReleaseAll,
        },
    )?;
    drain_input_events(&mut ble_events)?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_106,
            command: E2eCommand::Keyboard {
                modifiers: 0,
                keys: [0x04, 0, 0, 0, 0, 0],
            },
        },
    )?;
    wait_for_key_event(fallback_events, 30, 1, Duration::from_secs(3))?;
    assert_no_input_event(&mut ble_events, 1, 30, 1, Duration::from_millis(500))?;
    send_normalized(
        serial,
        E2ePacket {
            sequence: 1_107,
            command: E2eCommand::ReleaseAll,
        },
    )?;
    wait_for_key_event(fallback_events, 30, 0, Duration::from_secs(3))?;
    run_ble_management(hidshiftctl, address, &["target", "status"])?;
    println!(
        "T06-T08 passed: BLE GATT Management switched both directions with release, suppression and no broadcast"
    );
    Ok(())
}

fn drain_input_events(files: &mut [fs::File]) -> Result<(), Box<dyn Error>> {
    let mut buffer = [0; 24 * 16];
    for file in files {
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

fn assert_no_input_event(
    files: &mut [fs::File],
    event_type: u16,
    event_code: u16,
    value: i32,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    const INPUT_EVENT_LEN: usize = 24;
    let deadline = Instant::now() + timeout;
    let mut bytes = [0; INPUT_EVENT_LEN * 8];
    while Instant::now() < deadline {
        for file in files.iter_mut() {
            match file.read(&mut bytes) {
                Ok(length) => {
                    for event in bytes[..length].chunks_exact(INPUT_EVENT_LEN) {
                        if u16::from_ne_bytes([event[16], event[17]]) == event_type
                            && u16::from_ne_bytes([event[18], event[19]]) == event_code
                            && i32::from_ne_bytes([event[20], event[21], event[22], event[23]])
                                == value
                        {
                            return Err(format!(
                                "unexpected evdev type {event_type} code {event_code} value {value}"
                            )
                            .into());
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error.into()),
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
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
    let mut sent = false;
    for file in files {
        match file.write_all(&events) {
            Ok(()) => sent = true,
            Err(error) => last_error = Some(error),
        }
    }
    if sent {
        return Ok(());
    }
    Err(last_error.map_or_else(|| "no keyboard evdev node".into(), |error| error.into()))
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
    Err(format!("no hidraw node found for {vid:04x}:{pid:04x} interface {interface_number}").into())
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
                        if observed_type == event_type && code == event_code && event_value == value
                        {
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
            MirrorE2ePacket::new(OPCODE_REGISTER_CHUNK, *sequence, transfer_id, offset, chunk)
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

fn wait_for_mirror_candidate(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
    expected_profile_hash: u32,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let pending = client
            .begin(ManagementCommand::GetMirrorCandidate(MirrorCandidateId(0)))
            .map_err(|error| format!("GET_MIRROR_CANDIDATE request: {error:?}"))?;
        serial.write_all(&encode_serial_request(pending))?;
        let response = wait_management_response(serial, client, Duration::from_secs(1))?;
        if response.result == ManagementResult::Ok
            && matches!(
                response.payload,
                ManagementResponsePayload::MirrorCandidate(candidate)
                    if candidate.profile_hash == expected_profile_hash
            )
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    Err(format!("Mirror candidate 0 did not publish profile {expected_profile_hash:08x}").into())
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

fn pair_linux_ble_peer(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
    address: &str,
    host_slot: u8,
) -> Result<(), Box<dyn Error>> {
    // hardware-e2e intentionally uses volatile Host state, so flashing always
    // requires a fresh pairing. Remove a stale BlueZ bond before opening the
    // firmware pairing window; otherwise each side can retain different keys.
    let _ = Command::new("bluetoothctl")
        .args(["remove", address])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    send_management_command(
        serial,
        client,
        ManagementCommand::StartPairing(HostId(host_slot)),
        "START_PAIRING",
    )?;

    let mut bluez = Command::new("bluetoothctl")
        .args(["--agent", "NoInputNoOutput"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("start bluetoothctl: {error}"))?;
    let mut bluez_stdin = bluez
        .stdin
        .take()
        .ok_or("bluetoothctl stdin was not available")?;
    std::thread::sleep(Duration::from_secs(1));
    bluez_stdin.write_all(b"default-agent\nscan on\n")?;
    bluez_stdin.flush()?;
    let discovery_deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < discovery_deadline {
        let devices = Command::new("bluetoothctl").arg("devices").output()?;
        if String::from_utf8_lossy(&devices.stdout).contains(address) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let devices = Command::new("bluetoothctl").arg("devices").output()?;
    if !String::from_utf8_lossy(&devices.stdout).contains(address) {
        let _ = bluez.kill();
        let _ = bluez.wait();
        return Err(format!("BlueZ did not discover HIDShift peer {address}").into());
    }
    writeln!(bluez_stdin, "pair {address}")?;
    bluez_stdin.flush()?;
    std::thread::sleep(Duration::from_secs(12));
    writeln!(bluez_stdin, "trust {address}\nscan off\nquit")?;
    drop(bluez_stdin);
    let bluez_output = bluez
        .wait_with_output()
        .map_err(|error| format!("wait for bluetoothctl pairing: {error}"))?;
    if !bluez_output.status.success() {
        return Err(format!(
            "BlueZ pairing failed for {address}: {}{}",
            String::from_utf8_lossy(&bluez_output.stdout),
            String::from_utf8_lossy(&bluez_output.stderr)
        )
        .into());
    }
    let info = Command::new("bluetoothctl")
        .args(["info", address])
        .output()
        .map_err(|error| format!("inspect BLE peer {address}: {error}"))?;
    let info_text = String::from_utf8_lossy(&info.stdout);
    if !info.status.success()
        || !info_text.contains("Paired: yes")
        || !info_text.contains("Bonded: yes")
    {
        return Err(format!(
            "BlueZ did not retain a bond for {address}: {info_text}\npair session: {}{}",
            String::from_utf8_lossy(&bluez_output.stdout),
            String::from_utf8_lossy(&bluez_output.stderr)
        )
        .into());
    }
    println!("BLE setup: paired Linux with HIDShift Host slot {host_slot}");
    Ok(())
}

fn wait_for_management_ready(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let pending = client
            .begin(ManagementCommand::GetOutputTargetStatus)
            .map_err(|error| format!("management readiness request: {error:?}"))?;
        serial.write_all(&encode_serial_request(pending))?;
        match wait_management_response(serial, client, Duration::from_secs(1)) {
            Ok(response) if response.result == ManagementResult::Ok => return Ok(()),
            Ok(_) | Err(_) => {
                client.cancel();
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
    Err("Host S3 management runtime did not become ready".into())
}

fn output_target_status(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
) -> Result<hidshift::ManagementOutputTargetStatus, Box<dyn Error>> {
    let pending = client
        .begin(ManagementCommand::GetOutputTargetStatus)
        .map_err(|error| format!("GET_OUTPUT_TARGET_STATUS request: {error:?}"))?;
    serial.write_all(&encode_serial_request(pending))?;
    let response = wait_management_response(serial, client, Duration::from_secs(2))?;
    match (response.result, response.payload) {
        (ManagementResult::Ok, ManagementResponsePayload::OutputTargetStatus(status)) => Ok(status),
        (result, payload) => Err(format!(
            "GET_OUTPUT_TARGET_STATUS failed: result={result:?} payload={payload:?}"
        )
        .into()),
    }
}

fn wait_for_wired_ready(
    serial: &mut dyn SerialPort,
    client: &mut ManagementClient,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(status) = output_target_status(serial, client)
            && status.selected == ManagementOutputTarget::Wired
            && status.active == Some(ManagementOutputTarget::Wired)
            && status.wired_ready
        {
            return Ok(());
        }
        client.cancel();
        std::thread::sleep(Duration::from_millis(100));
    }
    Err("Wired target did not recover after Device S3 reset".into())
}

fn send_mirror(serial: &mut dyn SerialPort, packet: MirrorE2ePacket) -> Result<(), Box<dyn Error>> {
    serial.write_all(&packet.encode_line())?;
    serial.write_all(b"\n")?;
    Ok(())
}

fn wait_for_mirror_ready(serial: &mut dyn SerialPort) -> Result<(), Box<dyn Error>> {
    let hello = MirrorE2ePacket::new(OPCODE_HELLO, 1, 0, 0, &[])
        .map_err(|error| format!("HELLO packet: {error:?}"))?;
    for _ in 0..8 {
        send_mirror(serial, hello)?;
        if wait_for_text(
            serial,
            b"@HIDSHIFT-MIRROR:READY,1,1",
            Duration::from_secs(1),
        )
        .is_ok()
        {
            return Ok(());
        }
    }
    Err("Host S3 did not answer Mirror E2E HELLO".into())
}

fn send_normalized(serial: &mut dyn SerialPort, packet: E2ePacket) -> Result<(), Box<dyn Error>> {
    serial.write_all(&packet.encode_line())?;
    serial.write_all(b"\n")?;
    Ok(())
}

fn reset_device_s3(port: &Path) -> Result<(), Box<dyn Error>> {
    let status = Command::new("espflash")
        .args(["board-info", "--port"])
        .arg(port)
        .args(["--chip", "esp32s3"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(format!("espflash failed to reset Device S3 on {}", port.display()).into());
    }
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
                if line
                    .windows(expected.len())
                    .any(|window| window == expected)
                {
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

fn verify_usb_plan(plan: &UsbDevicePlan<'_>) -> Result<(), Box<dyn Error>> {
    let (vid, pid) = plan_identity(&plan.device_descriptor);
    let path = find_usb_device(vid, pid)?.ok_or_else(|| {
        format!("USB identity {vid:04x}:{pid:04x} disappeared before descriptor verification")
    })?;
    let raw = fs::read(path.join("descriptors"))?;
    verify_raw_descriptors(plan, &raw)?;
    if !plan.bos_descriptor.is_empty() {
        let actual_bos = read_usb_bos_descriptor(vid, pid)?;
        match actual_bos {
            Some(actual) if actual == plan.bos_descriptor => {}
            None => {
                return Err("USB device did not expose the MirrorImage BOS Descriptor".into());
            }
            Some(_) => {
                return Err("raw USB BOS Descriptor does not match MirrorImage".into());
            }
        }
    }
    verify_usb_strings(plan, &path)?;
    for interface in &plan.interfaces {
        let interface_path = PathBuf::from(format!(
            "{}:1.{}",
            path.to_string_lossy(),
            interface.interface_number
        ));
        let report_path =
            find_named_file(&interface_path, "report_descriptor", 3)?.ok_or_else(|| {
                format!(
                    "no Linux HID report_descriptor for USB interface {}",
                    interface.interface_number
                )
            })?;
        let actual = fs::read(&report_path)?;
        if actual != interface.report_descriptor {
            return Err(format!(
                "HID Report Descriptor mismatch for interface {} at {}",
                interface.interface_number,
                report_path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn verify_raw_descriptors(plan: &UsbDevicePlan<'_>, raw: &[u8]) -> Result<(), Box<dyn Error>> {
    if raw.get(..18) != Some(plan.device_descriptor.as_slice()) {
        return Err("raw USB Device Descriptor does not match MirrorImage".into());
    }
    let configuration_length = plan.configuration_descriptor.len();
    if raw.get(18..18 + configuration_length) != Some(plan.configuration_descriptor) {
        return Err("raw USB Configuration Descriptor does not match MirrorImage".into());
    }
    Ok(())
}

fn read_usb_bos_descriptor(vid: u16, pid: u16) -> Result<Option<Vec<u8>>, Box<dyn Error>> {
    const GET_DESCRIPTOR: u8 = 0x06;
    const BOS_DESCRIPTOR: u16 = 0x0f00;
    const DEVICE_TO_HOST_STANDARD_DEVICE: u8 = 0x80;

    let devices = rusb::devices()?;
    let device = devices
        .iter()
        .find(|device| {
            device.device_descriptor().is_ok_and(|descriptor| {
                descriptor.vendor_id() == vid && descriptor.product_id() == pid
            })
        })
        .ok_or_else(|| format!("libusb could not find {vid:04x}:{pid:04x}"))?;
    let handle = device.open()?;
    let mut header = [0u8; 5];
    let header_length = match handle.read_control(
        DEVICE_TO_HOST_STANDARD_DEVICE,
        GET_DESCRIPTOR,
        BOS_DESCRIPTOR,
        0,
        &mut header,
        Duration::from_secs(1),
    ) {
        Ok(length) => length,
        Err(rusb::Error::Pipe) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if header_length != header.len() || header[0] != 5 || header[1] != 0x0f {
        return Err(format!("malformed USB BOS header: {header:02x?}").into());
    }
    let total_length = usize::from(u16::from_le_bytes([header[2], header[3]]));
    if total_length < header.len() {
        return Err(format!("invalid USB BOS total length {total_length}").into());
    }
    let mut descriptor = vec![0; total_length];
    let actual_length = handle.read_control(
        DEVICE_TO_HOST_STANDARD_DEVICE,
        GET_DESCRIPTOR,
        BOS_DESCRIPTOR,
        0,
        &mut descriptor,
        Duration::from_secs(1),
    )?;
    if actual_length != total_length {
        return Err(format!(
            "short USB BOS Descriptor: expected {total_length}, received {actual_length}"
        )
        .into());
    }
    Ok(Some(descriptor))
}

fn verify_usb_strings(plan: &UsbDevicePlan<'_>, path: &Path) -> Result<(), Box<dyn Error>> {
    for (descriptor_index, sysfs_name) in [
        (plan.device_descriptor[14], "manufacturer"),
        (plan.device_descriptor[15], "product"),
        (plan.device_descriptor[16], "serial"),
    ] {
        if descriptor_index == 0 {
            continue;
        }
        let expected = plan
            .strings
            .get(descriptor_index, 0x0409)
            .and_then(decode_usb_string)
            .ok_or_else(|| format!("missing String Descriptor index {descriptor_index}"))?;
        let actual = fs::read_to_string(path.join(sysfs_name))?;
        if actual.trim_end() != expected {
            return Err(format!(
                "USB {sysfs_name} mismatch: expected {expected:?}, got {:?}",
                actual.trim_end()
            )
            .into());
        }
    }
    Ok(())
}

fn decode_usb_string(descriptor: &[u8]) -> Option<String> {
    if descriptor.len() < 2
        || !descriptor.len().is_multiple_of(2)
        || usize::from(descriptor[0]) != descriptor.len()
        || descriptor[1] != 0x03
    {
        return None;
    }
    let utf16 = descriptor[2..]
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]));
    char::decode_utf16(utf16)
        .collect::<Result<String, _>>()
        .ok()
}

fn find_usb_device(vid: u16, pid: u16) -> Result<Option<PathBuf>, Box<dyn Error>> {
    for entry in fs::read_dir("/sys/bus/usb/devices")? {
        let path = entry?.path();
        if read_hex(path.join("idVendor")) == Some(vid)
            && read_hex(path.join("idProduct")) == Some(pid)
        {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn find_named_file(
    root: &Path,
    name: &str,
    remaining_depth: usize,
) -> Result<Option<PathBuf>, Box<dyn Error>> {
    if remaining_depth == 0 || !root.is_dir() {
        return Ok(None);
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.file_name().and_then(|value| value.to_str()) == Some(name) && path.is_file() {
            return Ok(Some(path));
        }
        if path.is_dir()
            && let Some(found) = find_named_file(&path, name, remaining_depth - 1)?
        {
            return Ok(Some(found));
        }
    }
    Ok(None)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_hidraw_feature_requests_match_uapi_values() {
        assert_eq!(hidraw_feature_request(17, 0x07), 0xc011_4807);
        assert_eq!(hidraw_feature_request(17, 0x06), 0xc011_4806);
    }

    #[test]
    fn descriptor_verifier_requires_exact_device_and_configuration_bytes() {
        let image = include_bytes!("../../fixtures/mirror/composite-a.hsmi");
        let plan = validate_mirror_image(image).unwrap();
        let mut raw = Vec::from(plan.device_descriptor);
        raw.extend_from_slice(plan.configuration_descriptor);
        verify_raw_descriptors(&plan, &raw).unwrap();

        raw[12] ^= 1;
        assert!(verify_raw_descriptors(&plan, &raw).is_err());
        raw[12] ^= 1;
        raw[18 + 20] ^= 1;
        assert!(verify_raw_descriptors(&plan, &raw).is_err());
    }

    #[test]
    fn usb_string_decoder_preserves_utf16_content() {
        assert_eq!(
            decode_usb_string(&[8, 3, b'E', 0, b'2', 0, b'E', 0]),
            Some("E2E".into())
        );
        assert_eq!(decode_usb_string(&[3, 3, b'E']), None);
    }

    #[test]
    fn bluetooth_build_address_accepts_only_canonical_mac_text() {
        assert!(valid_bluetooth_address("4C:23:38:A6:20:44"));
        assert!(!valid_bluetooth_address("4C:23:38:A6:20"));
        assert!(!valid_bluetooth_address("4C:23:38:A6:20:'"));
    }
}
