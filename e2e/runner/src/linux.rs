use std::collections::VecDeque;

use super::*;

#[derive(Debug, Serialize)]
struct LinuxReport {
    schema_version: u8,
    unix_time_seconds: u64,
    dut_port: String,
    tests: Vec<TestResult>,
    keyboard_linux_observed: LatencyStats,
    keyboard_firmware: LatencyStats,
    mouse_linux_observed: LatencyStats,
    mouse_firmware: LatencyStats,
    stability: LinuxStabilityStats,
    keyboard_baseline_comparison: Option<BaselineComparison>,
    mouse_baseline_comparison: Option<BaselineComparison>,
}

struct LinuxLatencyMeasurement {
    observed: LatencyStats,
    firmware: LatencyStats,
}

#[derive(Debug, Serialize)]
struct LinuxStabilityStats {
    duration_seconds: f64,
    reports_sent: u64,
    reports_received: u64,
    mismatches: u64,
    timeouts: u64,
    last_timeout: Option<String>,
    dut_inputs: u32,
    dut_ble_queued: u32,
    dut_notify_done: u32,
    dut_counter_reset: bool,
}

pub(super) fn run_suite(args: &Args, repo: &Path) -> Result<()> {
    let dut = resolve_linux_dut(args, repo)?;
    println!("DUT   {} ({DUT_CHIP})", dut.display());
    println!("Input Linux BlueZ + evdev");
    if !args.skip_flash {
        build_and_flash_linux(repo, &dut)?;
    }

    let mut harness = open_dut_harness(&dut)?;
    wait_for_dut_readiness(&mut harness, Duration::from_secs(12))?;
    wait_for_usb_inventory_settled(&mut harness, Duration::from_secs(15))?;
    provision_linux_host(&mut harness)?;
    let mut input = LinuxInputObserver::open_bluetooth("HIDShift", Duration::from_secs(15))?;
    input.drain();

    let mut tests = run_linux_functional_tests(&mut harness, &mut input)?;
    input.drain();
    let (keyboard, mouse) =
        run_linux_latency_tests(&mut harness, &mut input, args.latency_samples)?;
    let stability = run_linux_stability_test(
        &mut harness,
        &mut input,
        Duration::from_secs(args.stability_seconds),
    )?;

    let baseline = read_baseline(&repo.join(&args.linux_baseline))?;
    let keyboard_baseline_comparison = baseline
        .as_ref()
        .map(|baseline| compare_baseline(&baseline.keyboard, &keyboard.observed));
    let mouse_baseline_comparison = baseline
        .as_ref()
        .map(|baseline| compare_baseline(&baseline.mouse, &mouse.observed));
    if let Some(comparison) = &keyboard_baseline_comparison {
        tests.push(TestResult {
            name: "linux_keyboard_latency_baseline_regression".into(),
            passed: comparison.passed,
            detail: format!(
                "p95 {:.3} ms -> {:.3} ms ({:+.1}%)",
                comparison.baseline_p95_ms, comparison.current_p95_ms, comparison.change_percent
            ),
        });
    }
    if let Some(comparison) = &mouse_baseline_comparison {
        tests.push(TestResult {
            name: "linux_mouse_latency_baseline_regression".into(),
            passed: comparison.passed,
            detail: format!(
                "p95 {:.3} ms -> {:.3} ms ({:+.1}%)",
                comparison.baseline_p95_ms, comparison.current_p95_ms, comparison.change_percent
            ),
        });
    }
    tests.push(TestResult {
        name: "linux_short_stability".into(),
        passed: stability.mismatches == 0
            && stability.timeouts == 0
            && !stability.dut_counter_reset
            && stability.dut_inputs == stability.reports_sent as u32
            && stability.dut_ble_queued == stability.reports_sent as u32
            && stability.dut_notify_done == stability.reports_sent as u32,
        detail: format!(
            "{}/{} events, mismatches={}, timeouts={}, counters={}/{}/{}",
            stability.reports_received,
            stability.reports_sent,
            stability.mismatches,
            stability.timeouts,
            stability.dut_inputs,
            stability.dut_ble_queued,
            stability.dut_notify_done
        ),
    });
    tests.push(TestResult {
        name: "linux_keyboard_firmware_latency".into(),
        passed: ble_game_latency_passes(&keyboard.firmware),
        detail: format!(
            "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
            keyboard.firmware.p50_ms, keyboard.firmware.p95_ms, keyboard.firmware.p99_ms
        ),
    });
    tests.push(TestResult {
        name: "linux_mouse_firmware_latency".into(),
        passed: ble_game_latency_passes(&mouse.firmware),
        detail: format!(
            "p50={:.3} ms p95={:.3} ms p99={:.3} ms",
            mouse.firmware.p50_ms, mouse.firmware.p95_ms, mouse.firmware.p99_ms
        ),
    });

    let report = LinuxReport {
        schema_version: 1,
        unix_time_seconds: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        dut_port: dut.display().to_string(),
        tests,
        keyboard_linux_observed: keyboard.observed,
        keyboard_firmware: keyboard.firmware,
        mouse_linux_observed: mouse.observed,
        mouse_firmware: mouse.firmware,
        stability,
        keyboard_baseline_comparison,
        mouse_baseline_comparison,
    };
    let results_dir = repo.join(&args.results_dir);
    fs::create_dir_all(&results_dir)?;
    let result_path = results_dir.join(format!("{}-linux.json", report.unix_time_seconds));
    fs::write(&result_path, serde_json::to_vec_pretty(&report)?)?;
    if args.write_linux_baseline {
        let baseline_path = repo.join(&args.linux_baseline);
        if let Some(parent) = baseline_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(
            &baseline_path,
            serde_json::to_vec_pretty(&PerformanceBaseline {
                schema_version: 2,
                metric: "linux_uart_injection_to_evdev".into(),
                keyboard: report.keyboard_linux_observed.clone(),
                mouse: report.mouse_linux_observed.clone(),
            })?,
        )?;
    }
    println!("\n{}", serde_json::to_string_pretty(&report)?);
    println!("result: {}", result_path.display());
    ensure!(
        report.tests.iter().all(|test| test.passed),
        "one or more Linux E2E tests failed"
    );
    Ok(())
}

fn resolve_linux_dut(args: &Args, repo: &Path) -> Result<PathBuf> {
    ensure!(
        args.probe_port.is_none() && args.probe_chip.is_none(),
        "--linux-only does not use Probe arguments"
    );
    if let Some(dut) = &args.dut_port {
        if !args.skip_flash {
            verify_chip(repo, dut, DUT_CHIP)?;
        }
        return Ok(dut.clone());
    }
    ensure!(
        !args.skip_flash,
        "--reuse-firmware --linux-only requires --dut-port to avoid resetting boards during discovery"
    );
    for path in serial_by_path_candidates(Path::new("/dev/serial/by-path"))? {
        if let Ok(info) = board_info(repo, &path)
            && parse_chip_type(&info).as_deref() == Some(DUT_CHIP)
            && parse_mac_address(&info).is_some_and(|mac| mac.eq_ignore_ascii_case(DUT_MAC))
        {
            return Ok(path);
        }
    }
    bail!("the configured ESP32-S3 DUT MAC was not found")
}

fn build_and_flash_linux(repo: &Path, dut: &Path) -> Result<()> {
    let export = esp_export_path()?;
    remove_cached_hidshift_devices();
    let linux_address = linux_controller_address()?;
    let _ = bluetoothctl(&["power", "off"], 10);
    let build = format!(
        ". '{}' && HIDSHIFT_E2E_LINUX_ADDRESS='{}' HIDSHIFT_E2E_LINUX_ONLY=1 cargo +esp build --locked -Zbuild-std=core,alloc --release --manifest-path firmware/Cargo.toml --bin firmware --features hardware-e2e --target xtensa-esp32s3-none-elf",
        export.display(),
        linux_address
    );
    run(Command::new("sh").arg("-c").arg(build), repo)?;
    run(
        Command::new("espflash")
            .args(["flash", "--chip", DUT_CHIP, "--port"])
            .arg(dut)
            .args([
                "--partition-table",
                "partitions/bridge.csv",
                "--target-app-partition",
                "ota_0",
                "target/xtensa-esp32s3-none-elf/release/firmware",
            ]),
        repo,
    )?;
    run(
        Command::new("espflash")
            .args(["erase-parts", "--chip", DUT_CHIP, "--port"])
            .arg(dut)
            .args(["--partition-table", "partitions/bridge.csv", "bridge"]),
        repo,
    )?;
    Ok(())
}

fn provision_linux_host(harness: &mut Harness) -> Result<()> {
    power_on_linux_bluetooth()?;
    let status = read_management_status(harness)?;
    if !status.hosts[0].bonded {
        start_pairing(harness, HostId(1))?;
    }
    println!("provisioning: LinuxAdvertising");
    let address = discover_hidshift_address()?;
    if !status.hosts[0].bonded {
        println!("provisioning: LinuxPair");
        pair_linux_host(&address, || start_pairing(harness, HostId(1)))?;
        wait_for_host_bond(harness, HostId(1), Duration::from_secs(15))?;
    }
    bluetoothctl(&["trust", &address], 10)?;
    println!("provisioning: LinuxLink");
    wait_linux_link(&address, Duration::from_secs(60))?;
    let request_id = harness.send_management(ManagementCommand::SelectHost(HostId(1)))?;
    let response = harness.wait_management_response(request_id, Duration::from_secs(3))?;
    ensure!(
        response.result == ManagementResult::Ok,
        "DUT rejected Linux host selection: {:?}",
        response.result
    );
    thread::sleep(Duration::from_secs(2));
    let status = read_management_status(harness)?;
    ensure!(
        status.hosts[0].connected
            && status.hosts[0].encrypted
            && status.active_host == Some(HostId(1)),
        "Linux host did not become the active encrypted session"
    );
    Ok(())
}

fn wait_for_usb_inventory_settled(harness: &mut Harness, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut previous = None;
    let mut stable_samples = 0;
    while Instant::now() < deadline {
        let status = read_management_status(harness)?;
        let current = (
            status.usb.device_count,
            status.usb.interface_count,
            status.usb.keyboard_count,
        );
        if current.0 > 0 && current.1 > 0 && Some(current) == previous {
            stable_samples += 1;
            if stable_samples >= 3 {
                return Ok(());
            }
        } else {
            stable_samples = 0;
        }
        previous = Some(current);
        thread::sleep(Duration::from_millis(500));
    }
    bail!("DUT USB inventory did not settle before Linux pairing")
}

fn wait_for_host_bond(harness: &mut Harness, host_id: HostId, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let index = usize::from(host_id.0 - 1);
    while Instant::now() < deadline {
        let status = read_management_status(harness)?;
        if status.hosts[index].bonded {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(500));
    }
    bail!("DUT did not persist the Linux host bond")
}

const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const KEY_A: u16 = 30;
const KEY_B: u16 = 48;
const KEY_C: u16 = 46;
const KEY_D: u16 = 32;
const KEY_E: u16 = 18;
const KEY_F: u16 = 33;
const KEY_LEFT_SHIFT: u16 = 42;
const KEY_VOLUME_UP: u16 = 115;
const BTN_LEFT: u16 = 272;
const BTN_RIGHT: u16 = 273;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;

fn run_linux_functional_tests(
    harness: &mut Harness,
    input: &mut LinuxInputObserver,
) -> Result<Vec<TestResult>> {
    harness.send(E2eCommand::ReleaseAll)?;
    thread::sleep(Duration::from_millis(100));
    input.drain();
    let mut tests = Vec::new();

    harness.send(E2eCommand::Keyboard {
        modifiers: 0,
        keys: [4, 0, 0, 0, 0, 0],
    })?;
    input.wait_for(EV_KEY, KEY_A, 1, Duration::from_secs(2))?;
    harness.send(E2eCommand::ReleaseAll)?;
    input.wait_for(EV_KEY, KEY_A, 0, Duration::from_secs(2))?;
    tests.push(TestResult {
        name: "linux_keyboard_press_release".into(),
        passed: true,
        detail: "KEY_A press/release reached evdev".into(),
    });

    input.drain();
    harness.send(E2eCommand::Keyboard {
        modifiers: 0x02,
        keys: [4, 5, 6, 7, 8, 9],
    })?;
    let six_key_press = [
        (EV_KEY, KEY_LEFT_SHIFT, 1),
        (EV_KEY, KEY_A, 1),
        (EV_KEY, KEY_B, 1),
        (EV_KEY, KEY_C, 1),
        (EV_KEY, KEY_D, 1),
        (EV_KEY, KEY_E, 1),
        (EV_KEY, KEY_F, 1),
    ];
    input.wait_for_all(&six_key_press, Duration::from_secs(2))?;
    harness.send(E2eCommand::ReleaseAll)?;
    let six_key_release = six_key_press.map(|(event_type, code, _)| (event_type, code, 0));
    input.wait_for_all(&six_key_release, Duration::from_secs(2))?;
    tests.push(TestResult {
        name: "linux_keyboard_modifier_6kro".into(),
        passed: true,
        detail: "Left Shift + A-F press/release reached evdev".into(),
    });

    input.drain();
    harness.send(E2eCommand::Mouse {
        buttons: 3,
        x: 10,
        y: -7,
        wheel: 2,
        pan: -1,
    })?;
    input.wait_for_all(
        &[
            (EV_KEY, BTN_LEFT, 1),
            (EV_KEY, BTN_RIGHT, 1),
            (EV_REL, REL_X, 10),
            (EV_REL, REL_Y, -7),
            (EV_REL, REL_WHEEL, 2),
            (EV_REL, REL_HWHEEL, -1),
        ],
        Duration::from_secs(2),
    )?;
    harness.send(E2eCommand::ReleaseAll)?;
    input.wait_for_all(
        &[(EV_KEY, BTN_LEFT, 0), (EV_KEY, BTN_RIGHT, 0)],
        Duration::from_secs(2),
    )?;
    tests.push(TestResult {
        name: "linux_mouse".into(),
        passed: true,
        detail: "buttons, X/Y, wheel and pan reached evdev".into(),
    });

    input.drain();
    harness.send(E2eCommand::Consumer { usage: 0x00e9 })?;
    input.wait_for(EV_KEY, KEY_VOLUME_UP, 1, Duration::from_secs(2))?;
    harness.send(E2eCommand::ReleaseAll)?;
    input.wait_for(EV_KEY, KEY_VOLUME_UP, 0, Duration::from_secs(2))?;
    tests.push(TestResult {
        name: "linux_consumer".into(),
        passed: true,
        detail: "Volume Up press/release reached evdev".into(),
    });
    Ok(tests)
}

fn run_linux_latency_tests(
    harness: &mut Harness,
    input: &mut LinuxInputObserver,
    samples: usize,
) -> Result<(LinuxLatencyMeasurement, LinuxLatencyMeasurement)> {
    ensure!(samples > 0, "--latency-samples must be positive");
    let mut keyboard_observed = Vec::with_capacity(samples * 2);
    let mut keyboard_firmware = Vec::with_capacity(samples * 2);
    let mut mouse_observed = Vec::with_capacity(samples);
    let mut mouse_firmware = Vec::with_capacity(samples);
    input.drain();
    for index in 0..samples {
        thread::sleep(sample_phase_delay(index * 3));
        let (sequence, started) = harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: [4, 0, 0, 0, 0, 0],
        })?;
        let event = input.wait_for(EV_KEY, KEY_A, 1, Duration::from_secs(2))?;
        record_linux_latency(
            harness,
            sequence,
            started,
            event,
            &mut keyboard_observed,
            &mut keyboard_firmware,
        )?;

        thread::sleep(sample_phase_delay(index * 3 + 1));
        let (sequence, started) = harness.send(E2eCommand::Keyboard {
            modifiers: 0,
            keys: [0; 6],
        })?;
        let event = input.wait_for(EV_KEY, KEY_A, 0, Duration::from_secs(2))?;
        record_linux_latency(
            harness,
            sequence,
            started,
            event,
            &mut keyboard_observed,
            &mut keyboard_firmware,
        )?;

        let x = if index % 2 == 0 { 1 } else { -1 };
        thread::sleep(sample_phase_delay(index * 3 + 2));
        let (sequence, started) = harness.send(E2eCommand::Mouse {
            buttons: 0,
            x,
            y: 0,
            wheel: 0,
            pan: 0,
        })?;
        let event = input.wait_for(EV_REL, REL_X, i32::from(x), Duration::from_secs(2))?;
        record_linux_latency(
            harness,
            sequence,
            started,
            event,
            &mut mouse_observed,
            &mut mouse_firmware,
        )?;
    }
    Ok((
        LinuxLatencyMeasurement {
            observed: latency_stats(keyboard_observed),
            firmware: latency_stats(keyboard_firmware),
        },
        LinuxLatencyMeasurement {
            observed: latency_stats(mouse_observed),
            firmware: latency_stats(mouse_firmware),
        },
    ))
}

fn record_linux_latency(
    harness: &mut Harness,
    sequence: u32,
    started: Instant,
    event: ObservedInputEvent,
    observed: &mut Vec<f64>,
    firmware: &mut Vec<f64>,
) -> Result<()> {
    observed.push(event.received.duration_since(started).as_secs_f64() * 1_000.0);
    firmware.push(firmware_latency_ms(read_dut_input_timestamp(
        harness, sequence,
    )?)?);
    Ok(())
}

fn run_linux_stability_test(
    harness: &mut Harness,
    input: &mut LinuxInputObserver,
    duration: Duration,
) -> Result<LinuxStabilityStats> {
    input.drain();
    let started = Instant::now();
    let dut_before = read_dut_snapshot(harness)?;
    let mut sent = 0u64;
    let mut received = 0u64;
    let mut timeouts = 0u64;
    let mut last_timeout = None;
    let mut index = 0u64;
    while started.elapsed() < duration {
        let (usage, code) = if index.is_multiple_of(2) {
            (4, KEY_A)
        } else {
            (5, KEY_B)
        };
        index += 1;
        for (keys, value) in [([usage, 0, 0, 0, 0, 0], 1), ([0; 6], 0)] {
            harness.send(E2eCommand::Keyboard { modifiers: 0, keys })?;
            sent += 1;
            match input.wait_for(EV_KEY, code, value, Duration::from_millis(500)) {
                Ok(_) => received += 1,
                Err(error) => {
                    timeouts += 1;
                    last_timeout = Some(error.to_string());
                }
            }
        }
    }
    let dut_after = read_dut_snapshot(harness)?;
    let dut_counter_reset = dut_after.input_count < dut_before.input_count
        || dut_after.ble_queued_count < dut_before.ble_queued_count
        || dut_after.notify_done_count < dut_before.notify_done_count;
    Ok(LinuxStabilityStats {
        duration_seconds: started.elapsed().as_secs_f64(),
        reports_sent: sent,
        reports_received: received,
        mismatches: sent.saturating_sub(received + timeouts),
        timeouts,
        last_timeout,
        dut_inputs: dut_after.input_count.saturating_sub(dut_before.input_count),
        dut_ble_queued: dut_after
            .ble_queued_count
            .saturating_sub(dut_before.ble_queued_count),
        dut_notify_done: dut_after
            .notify_done_count
            .saturating_sub(dut_before.notify_done_count),
        dut_counter_reset,
    })
}

struct ObservedInputEvent {
    event_type: u16,
    code: u16,
    value: i32,
    received: Instant,
}

struct LinuxInputFile {
    file: File,
    pending: Vec<u8>,
}

struct LinuxInputObserver {
    inputs: Vec<LinuxInputFile>,
    events: VecDeque<ObservedInputEvent>,
}

impl LinuxInputObserver {
    fn open_bluetooth(name: &str, timeout: Duration) -> Result<Self> {
        const BUS_BLUETOOTH: &str = "0005";
        let paths = find_evdevs_on_bus(name, Some(BUS_BLUETOOTH), timeout)?;
        let inputs = paths
            .iter()
            .map(|path| {
                Ok(LinuxInputFile {
                    file: open_nonblocking(path)?,
                    pending: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            inputs,
            events: VecDeque::new(),
        })
    }

    fn drain(&mut self) {
        self.events.clear();
        for input in &mut self.inputs {
            input.pending.clear();
            drain_file(&mut input.file);
        }
    }

    fn wait_for(
        &mut self,
        event_type: u16,
        code: u16,
        value: i32,
        timeout: Duration,
    ) -> Result<ObservedInputEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            while let Some(event) = self.events.pop_front() {
                if (event.event_type, event.code, event.value) == (event_type, code, value) {
                    return Ok(event);
                }
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for Linux input type={event_type} code={code} value={value}"
                );
            }
            self.read_available()?;
            if self.events.is_empty() {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn wait_for_all(&mut self, expected: &[(u16, u16, i32)], timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut remaining = expected.to_vec();
        while !remaining.is_empty() {
            let duration = deadline
                .checked_duration_since(Instant::now())
                .context("timed out waiting for Linux input event set")?;
            let event = self.wait_for_any(duration)?;
            if let Some(index) = remaining
                .iter()
                .position(|candidate| *candidate == (event.event_type, event.code, event.value))
            {
                remaining.swap_remove(index);
            }
        }
        Ok(())
    }

    fn wait_for_any(&mut self, timeout: Duration) -> Result<ObservedInputEvent> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(event);
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for Linux input event");
            }
            self.read_available()?;
            if self.events.is_empty() {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn read_available(&mut self) -> Result<()> {
        for input in &mut self.inputs {
            let mut buffer = [0; 256];
            loop {
                match input.file.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        input.pending.extend_from_slice(&buffer[..count]);
                        let received = Instant::now();
                        self.events
                            .extend(parse_input_events(&mut input.pending, received));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(())
    }
}

fn parse_input_events(pending: &mut Vec<u8>, received: Instant) -> Vec<ObservedInputEvent> {
    let event_len = std::mem::size_of::<libc::timeval>() + 8;
    let complete = pending.len() / event_len;
    let mut events = Vec::with_capacity(complete);
    for event in pending[..complete * event_len].chunks_exact(event_len) {
        let offset = std::mem::size_of::<libc::timeval>();
        events.push(ObservedInputEvent {
            event_type: u16::from_ne_bytes([event[offset], event[offset + 1]]),
            code: u16::from_ne_bytes([event[offset + 2], event[offset + 3]]),
            value: i32::from_ne_bytes([
                event[offset + 4],
                event[offset + 5],
                event[offset + 6],
                event[offset + 7],
            ]),
            received,
        });
    }
    pending.drain(..complete * event_len);
    events
}

fn firmware_latency_ms(timestamps: DutInputTimestamps) -> Result<f64> {
    ensure!(
        timestamps.ingress_us <= timestamps.hci_submit_us,
        "DUT HCI submit precedes ingress"
    );
    Ok((timestamps.hci_submit_us - timestamps.ingress_us) as f64 / 1_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_parser_preserves_complete_events_and_partial_tail() {
        let event_len = std::mem::size_of::<libc::timeval>() + 8;
        let mut bytes = vec![0; event_len * 2 + 3];
        let offset = std::mem::size_of::<libc::timeval>();
        bytes[offset..offset + 2].copy_from_slice(&1u16.to_ne_bytes());
        bytes[offset + 2..offset + 4].copy_from_slice(&30u16.to_ne_bytes());
        bytes[offset + 4..offset + 8].copy_from_slice(&1i32.to_ne_bytes());
        let second = event_len + offset;
        bytes[second..second + 2].copy_from_slice(&2u16.to_ne_bytes());
        bytes[second + 2..second + 4].copy_from_slice(&0u16.to_ne_bytes());
        bytes[second + 4..second + 8].copy_from_slice(&(-1i32).to_ne_bytes());
        let received = Instant::now();

        let events = parse_input_events(&mut bytes, received);

        assert_eq!(events.len(), 2);
        assert_eq!(
            (events[0].event_type, events[0].code, events[0].value),
            (1, 30, 1)
        );
        assert_eq!(
            (events[1].event_type, events[1].code, events[1].value),
            (2, 0, -1)
        );
        assert!(events.iter().all(|event| event.received == received));
        assert_eq!(bytes, vec![0; 3]);
    }

    #[test]
    fn firmware_latency_uses_one_dut_clock() {
        let timestamps = parse_dut_input_timestamp(
            "@HIDSHIFT-E2E:STAMP,43,42,1000,1050,1060,1065,1070,1080,1090,7,6,5,1,7500,0,2000,2,2,1,1,3250,1091,1093",
        )
        .unwrap();

        assert_eq!(firmware_latency_ms(timestamps).unwrap(), 2.25);
    }
}
