use std::collections::{BTreeMap, BTreeSet};

use embassy_usb_host::class::hid::ReportDescriptor as EmbassyReportDescriptor;
use hidreport::{Field, Report, ReportDescriptor};
use hidshift::usb_hid::output::{KeyboardLedOutputError, KeyboardLedOutputReport};
use hidshift::usb_hid::report::HidReportDescriptor;

// Keep fixture coverage aligned with the fixed capacities used by production firmware.
const EMBASSY_FIELDS_MAX: usize = 48;
const CORE_FIELDS_MAX: usize = 48;

struct PublicFixture {
    name: &'static str,
    bytes: &'static [u8],
}

const FIXTURES: &[PublicFixture] = &[
    PublicFixture {
        name: "0003-045E-00DB.0003.hid.bin",
        bytes: include_bytes!(
            "../third_party/hidreport-fixtures/tests/data/0003-045E-00DB.0003.hid.bin"
        ),
    },
    PublicFixture {
        name: "0003-045E-00DB.0004.hid.bin",
        bytes: include_bytes!(
            "../third_party/hidreport-fixtures/tests/data/0003-045E-00DB.0004.hid.bin"
        ),
    },
    PublicFixture {
        name: "0003-045E-0024.0004.hid.bin",
        bytes: include_bytes!(
            "../third_party/hidreport-fixtures/tests/data/0003-045E-0024.0004.hid.bin"
        ),
    },
    PublicFixture {
        name: "0003-045E-0745.0002.hid.bin",
        bytes: include_bytes!(
            "../third_party/hidreport-fixtures/tests/data/0003-045E-0745.0002.hid.bin"
        ),
    },
    PublicFixture {
        name: "0003-045E-07A9.000E.hid.bin",
        bytes: include_bytes!(
            "../third_party/hidreport-fixtures/tests/data/0003-045E-07A9.000E.hid.bin"
        ),
    },
    PublicFixture {
        name: "libinput-issue510-0005-057E-0306-0.rdesc",
        bytes: include_bytes!(
            "../third_party/hidreport-fixtures/tests/data/libinput-issue510-0005-057E-0306-0.rdesc"
        ),
    },
];

#[derive(Debug, Default, Eq, PartialEq)]
struct DescriptorSummary {
    input_report_lens: BTreeMap<u8, usize>,
    output_report_lens: BTreeMap<u8, usize>,
    supported_report_ids: BTreeSet<u8>,
    keyboard: bool,
    mouse: bool,
    consumer: bool,
    led_output: bool,
    wheel: bool,
    pan: bool,
}

impl DescriptorSummary {
    fn has_supported_domain(&self) -> bool {
        self.keyboard || self.mouse || self.consumer
    }
}

#[test]
fn public_hid_fixtures_do_not_panic() {
    for fixture in FIXTURES {
        let _ = ReportDescriptor::try_from(fixture.bytes).unwrap_or_else(|error| {
            panic!("hidreport parse failed for {}: {:?}", fixture.name, error)
        });
        let embassy = EmbassyReportDescriptor::<EMBASSY_FIELDS_MAX>::parse(fixture.bytes);
        let _ = to_core_descriptor::<EMBASSY_FIELDS_MAX, CORE_FIELDS_MAX>(&embassy);
    }
}

#[test]
fn unsupported_public_fixtures_are_safely_ignored() {
    for fixture in FIXTURES {
        let oracle = summary_from_hidreport(fixture.name, fixture.bytes);
        if oracle.has_supported_domain() {
            continue;
        }

        let embassy = EmbassyReportDescriptor::<EMBASSY_FIELDS_MAX>::parse(fixture.bytes);
        let core = summary_from_core_descriptor(
            &to_core_descriptor::<EMBASSY_FIELDS_MAX, CORE_FIELDS_MAX>(&embassy),
            fixture.bytes,
        );

        assert!(
            !core.has_supported_domain(),
            "unsupported fixture {} should remain ignored, got {:?}",
            fixture.name,
            core
        );
    }
}

#[test]
fn usb_descriptor_capabilities_match_hidreport_for_supported_fixtures() {
    for fixture in FIXTURES {
        let oracle = summary_from_hidreport(fixture.name, fixture.bytes);
        if !oracle.has_supported_domain() {
            continue;
        }

        let embassy = EmbassyReportDescriptor::<EMBASSY_FIELDS_MAX>::parse(fixture.bytes);
        let core = summary_from_core_descriptor(
            &to_core_descriptor::<EMBASSY_FIELDS_MAX, CORE_FIELDS_MAX>(&embassy),
            fixture.bytes,
        );

        assert_eq!(
            core.keyboard, oracle.keyboard,
            "keyboard mismatch for {}",
            fixture.name
        );
        assert_eq!(
            core.mouse, oracle.mouse,
            "mouse mismatch for {}",
            fixture.name
        );
        assert_eq!(
            core.consumer, oracle.consumer,
            "consumer mismatch for {}",
            fixture.name
        );
        assert_eq!(
            core.wheel, oracle.wheel,
            "wheel mismatch for {}",
            fixture.name
        );
        assert_eq!(core.pan, oracle.pan, "pan mismatch for {}", fixture.name);
        assert_eq!(
            core.supported_report_ids, oracle.supported_report_ids,
            "report ids mismatch for {}",
            fixture.name
        );
        assert_eq!(
            core.input_report_lens, oracle.input_report_lens,
            "input report lengths mismatch for {}",
            fixture.name
        );
        assert_eq!(
            core.led_output, oracle.led_output,
            "LED output mismatch for {}",
            fixture.name
        );
        assert_eq!(
            core.output_report_lens, oracle.output_report_lens,
            "output report lengths mismatch for {}",
            fixture.name
        );
    }
}

fn summary_from_hidreport(name: &str, bytes: &[u8]) -> DescriptorSummary {
    let descriptor = ReportDescriptor::try_from(bytes)
        .unwrap_or_else(|error| panic!("hidreport parse failed for {}: {:?}", name, error));
    let mut summary = DescriptorSummary::default();

    for report in descriptor.input_reports() {
        let report_id = report.report_id().map(u8::from).unwrap_or(0);
        let mut report_supported = false;
        for field in report.fields() {
            match field {
                Field::Variable(field) => {
                    let page = u16::from(field.usage.usage_page);
                    let usage = u16::from(field.usage.usage_id);
                    if page == 0x07 {
                        summary.keyboard = true;
                        report_supported = true;
                    }
                    if page == 0x09 {
                        summary.mouse = true;
                        report_supported = true;
                    }
                    if page == 0x01 && (usage == 0x30 || usage == 0x31 || usage == 0x38) {
                        summary.mouse = true;
                        summary.wheel |= usage == 0x38;
                        report_supported = true;
                    }
                    if page == 0x0c {
                        if usage == 0x0238 {
                            summary.mouse = true;
                            summary.pan = true;
                        } else {
                            summary.consumer = true;
                        }
                        report_supported = true;
                    }
                }
                Field::Array(field) => {
                    if let Some(range) = field.usage_range() {
                        let page = u16::from(range.minimum().usage_page());
                        if page == 0x07 {
                            summary.keyboard = true;
                            report_supported = true;
                        }
                        if page == 0x0c {
                            summary.consumer = true;
                            report_supported = true;
                        }
                    }
                }
                Field::Constant(_) => {}
            }
        }

        if report_supported {
            summary.supported_report_ids.insert(report_id);
            summary
                .input_report_lens
                .insert(report_id, report.size_in_bytes());
        }
    }

    for report in descriptor.output_reports() {
        let report_id = report.report_id().map(u8::from).unwrap_or(0);
        let mut has_led = false;
        for field in report.fields() {
            if let Field::Variable(field) = field
                && u16::from(field.usage.usage_page) == 0x08
            {
                summary.led_output = true;
                has_led = true;
            }
        }
        if has_led {
            summary
                .output_report_lens
                .insert(report_id, report.size_in_bytes());
        }
    }

    summary
}

fn summary_from_core_descriptor<const N: usize>(
    descriptor: &HidReportDescriptor<N>,
    report_descriptor: &[u8],
) -> DescriptorSummary {
    let mut summary = DescriptorSummary::default();
    let report_ids = descriptor
        .fields()
        .map(|field| field.report_id)
        .collect::<BTreeSet<_>>();

    for report_id in report_ids {
        let domains = descriptor.report_domains(report_id);
        let mut supported = false;
        if domains.keyboard {
            summary.keyboard = true;
            supported = true;
        }
        if domains.mouse {
            summary.mouse = true;
            supported = true;
        }
        if domains.consumer {
            summary.consumer = true;
            supported = true;
        }
        for field in descriptor
            .fields()
            .filter(|field| field.report_id == report_id)
        {
            if field.usage_page == 0x01 && field.usage_min <= 0x38 && 0x38 <= field.usage_max {
                summary.wheel = true;
            }
            if field.usage_page == 0x0c
                && field.flags & 0x02 != 0
                && field.usage_min == 0x0238
                && field.usage_max == 0x0238
                && field.count == 1
            {
                summary.pan = true;
                summary.mouse = true;
                supported = true;
            }
        }
        if supported {
            summary.supported_report_ids.insert(report_id);
            summary.input_report_lens.insert(
                report_id,
                report_len_for_core_descriptor(descriptor, report_id),
            );
        }
    }

    match KeyboardLedOutputReport::from_report_descriptor(report_descriptor) {
        Ok(report) => {
            summary.led_output = true;
            summary
                .output_report_lens
                .insert(report.report_id.unwrap_or(0), report.byte_len);
        }
        Err(KeyboardLedOutputError::MissingLedUsages) => {}
        Err(error) => panic!("unexpected led output parse error: {:?}", error),
    }

    summary
}

fn report_len_for_core_descriptor<const N: usize>(
    descriptor: &HidReportDescriptor<N>,
    report_id: u8,
) -> usize {
    let max_bit = descriptor
        .fields()
        .filter(|field| field.report_id == report_id)
        .map(|field| field.bit_offset as usize + field.bit_size as usize * field.count as usize)
        .max()
        .unwrap_or(0);
    let payload_len = max_bit.div_ceil(8);
    if descriptor.has_report_ids {
        1 + payload_len
    } else {
        payload_len
    }
}

fn to_core_descriptor<const SRC: usize, const DST: usize>(
    descriptor: &EmbassyReportDescriptor<SRC>,
) -> HidReportDescriptor<DST> {
    let mut core_descriptor = HidReportDescriptor::new(descriptor.has_report_ids);
    for field in descriptor.fields() {
        core_descriptor
            .push(hidshift::usb_hid::report::ReportField {
                report_id: field.report_id,
                usage_page: field.usage_page,
                usage_min: field.usage_min,
                usage_max: field.usage_max,
                bit_offset: field.bit_offset,
                bit_size: field.bit_size,
                count: field.count,
                flags: field.flags,
                logical_min: field.logical_min,
                logical_max: field.logical_max,
            })
            .unwrap();
    }
    core_descriptor
}
