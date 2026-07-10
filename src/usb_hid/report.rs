use crate::input::MouseButton;

pub const USAGE_PAGE_GENERIC_DESKTOP: u16 = 0x01;
pub const USAGE_PAGE_KEYBOARD: u16 = 0x07;
pub const USAGE_PAGE_LED: u16 = 0x08;
pub const USAGE_PAGE_BUTTON: u16 = 0x09;
pub const USAGE_PAGE_CONSUMER: u16 = 0x0c;

pub const USAGE_X: u16 = 0x30;
pub const USAGE_Y: u16 = 0x31;
pub const USAGE_WHEEL: u16 = 0x38;
pub const USAGE_AC_PAN: u16 = 0x0238;

pub const FIELD_FLAG_CONSTANT: u8 = 0x01;
pub const FIELD_FLAG_VARIABLE: u8 = 0x02;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidReportError {
    ReportIdMissing,
    TooManyFields,
    TooManyEvents,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReportField {
    pub report_id: u8,
    pub usage_page: u16,
    pub usage_min: u16,
    pub usage_max: u16,
    pub bit_offset: u32,
    pub bit_size: u8,
    pub count: u16,
    pub flags: u8,
    pub logical_min: i32,
    pub logical_max: i32,
}

impl ReportField {
    pub const fn is_constant(self) -> bool {
        self.flags & FIELD_FLAG_CONSTANT != 0
    }

    pub const fn is_variable(self) -> bool {
        self.flags & FIELD_FLAG_VARIABLE != 0
    }

    fn usage_at(self, index: usize) -> Option<u16> {
        if index >= self.count as usize {
            return None;
        }

        self.usage_min.checked_add(index as u16).and_then(|usage| {
            if usage <= self.usage_max {
                Some(usage)
            } else {
                None
            }
        })
    }

    fn index_of_usage(self, usage: u16) -> Option<usize> {
        if usage < self.usage_min || usage > self.usage_max {
            return None;
        }

        let index = (usage - self.usage_min) as usize;
        if index < self.count as usize {
            Some(index)
        } else {
            None
        }
    }

    fn extract_bool(self, payload: &[u8], index: usize) -> Option<bool> {
        Some(self.extract_u32(payload, index)? != 0)
    }

    fn extract_u32(self, payload: &[u8], index: usize) -> Option<u32> {
        if self.bit_size == 0 || self.bit_size > 32 || index >= self.count as usize {
            return None;
        }

        let start = self.bit_offset as usize + index * self.bit_size as usize;
        let end = start.checked_add(self.bit_size as usize)?;
        if end > payload.len() * 8 {
            return None;
        }

        let mut value = 0u32;
        let mut bit = 0usize;
        while bit < self.bit_size as usize {
            let source_bit = start + bit;
            let byte = payload[source_bit / 8];
            let bit_value = (byte >> (source_bit % 8)) & 1;
            value |= (bit_value as u32) << bit;
            bit += 1;
        }

        Some(value)
    }

    fn extract_i32(self, payload: &[u8], index: usize) -> Option<i32> {
        let raw = self.extract_u32(payload, index)?;
        if self.logical_min < 0 && self.bit_size < 32 {
            let sign_bit = 1u32 << (self.bit_size - 1);
            if raw & sign_bit != 0 {
                let extension = !((1u32 << self.bit_size) - 1);
                return Some((raw | extension) as i32);
            }
        }

        Some(raw as i32)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidReportDescriptor<const N: usize> {
    len: usize,
    fields: [Option<ReportField>; N],
    pub has_report_ids: bool,
}

impl<const N: usize> HidReportDescriptor<N> {
    pub const fn new(has_report_ids: bool) -> Self {
        Self {
            len: 0,
            fields: [None; N],
            has_report_ids,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn push(&mut self, field: ReportField) -> Result<(), HidReportError> {
        if self.len == N {
            return Err(HidReportError::TooManyFields);
        }

        self.fields[self.len] = Some(field);
        self.len += 1;
        Ok(())
    }

    pub fn fields(&self) -> impl Iterator<Item = ReportField> + '_ {
        self.fields[..self.len].iter().filter_map(|field| *field)
    }

    pub fn report_id_for<'a>(&self, report: &'a [u8]) -> Result<(u8, &'a [u8]), HidReportError> {
        self.matching_payload(report)
    }

    pub fn report_domains(&self, report_id: u8) -> HidReportDomains {
        let mut domains = HidReportDomains::empty();

        for field in self.fields() {
            if field.report_id != report_id || field.is_constant() {
                continue;
            }

            match field.usage_page {
                USAGE_PAGE_KEYBOARD => domains.keyboard = true,
                USAGE_PAGE_BUTTON => domains.mouse = true,
                USAGE_PAGE_GENERIC_DESKTOP
                    if field.index_of_usage(USAGE_X).is_some()
                        || field.index_of_usage(USAGE_Y).is_some()
                        || field.index_of_usage(USAGE_WHEEL).is_some() =>
                {
                    domains.mouse = true;
                }
                USAGE_PAGE_GENERIC_DESKTOP => {}
                USAGE_PAGE_CONSUMER => {
                    if field.is_variable()
                        && field.usage_min == USAGE_AC_PAN
                        && field.usage_max == USAGE_AC_PAN
                        && field.count == 1
                    {
                        domains.mouse = true;
                    }
                    if field.usage_min != USAGE_AC_PAN
                        || field.usage_max != USAGE_AC_PAN
                        || field.count > 1
                        || !field.is_variable()
                    {
                        domains.consumer = true;
                    }
                }
                _ => {}
            }
        }

        domains
    }

    fn matching_payload<'a>(&self, report: &'a [u8]) -> Result<(u8, &'a [u8]), HidReportError> {
        if !self.has_report_ids {
            return Ok((0, report));
        }

        let Some((&report_id, payload)) = report.split_first() else {
            return Err(HidReportError::ReportIdMissing);
        };
        Ok((report_id, payload))
    }

    fn extract_i32(
        &self,
        payload: &[u8],
        report_id: u8,
        usage_page: u16,
        usage: u16,
    ) -> Option<i32> {
        for field in self.fields() {
            if field.report_id != report_id
                || field.is_constant()
                || !field.is_variable()
                || field.usage_page != usage_page
            {
                continue;
            }

            let Some(index) = field.index_of_usage(usage) else {
                continue;
            };
            return field.extract_i32(payload, index);
        }

        None
    }

    fn extract_bool(
        &self,
        payload: &[u8],
        report_id: u8,
        usage_page: u16,
        usage: u16,
    ) -> Option<bool> {
        for field in self.fields() {
            if field.report_id != report_id
                || field.is_constant()
                || !field.is_variable()
                || field.usage_page != usage_page
            {
                continue;
            }

            let Some(index) = field.index_of_usage(usage) else {
                continue;
            };
            return field.extract_bool(payload, index);
        }

        None
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HidReportDomains {
    pub keyboard: bool,
    pub mouse: bool,
    pub consumer: bool,
}

impl HidReportDomains {
    pub const fn empty() -> Self {
        Self {
            keyboard: false,
            mouse: false,
            consumer: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HidReportEvent {
    KeyboardUsageDown(u8),
    MouseButtonDown(MouseButton),
    MouseX(i32),
    MouseY(i32),
    MouseWheel(i32),
    MousePan(i32),
    ConsumerUsageDown(u16),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HidReportEvents<const N: usize> {
    len: usize,
    events: [Option<HidReportEvent>; N],
}

impl<const N: usize> HidReportEvents<N> {
    pub const fn new() -> Self {
        Self {
            len: 0,
            events: [None; N],
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = HidReportEvent> + '_ {
        self.events[..self.len].iter().filter_map(|event| *event)
    }

    pub fn clear(&mut self) {
        let mut index = 0;
        while index < self.len {
            self.events[index] = None;
            index += 1;
        }
        self.len = 0;
    }

    fn push(&mut self, event: HidReportEvent) -> Result<(), HidReportError> {
        if self.len == N {
            return Err(HidReportError::TooManyEvents);
        }

        self.events[self.len] = Some(event);
        self.len += 1;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn push_for_test(&mut self, event: HidReportEvent) {
        self.push(event).unwrap();
    }
}

impl<const N: usize> Default for HidReportEvents<N> {
    fn default() -> Self {
        Self::new()
    }
}

pub fn decode_report<const FIELDS: usize, const EVENTS: usize>(
    descriptor: &HidReportDescriptor<FIELDS>,
    report: &[u8],
    events: &mut HidReportEvents<EVENTS>,
) -> Result<(), HidReportError> {
    events.clear();
    let (report_id, payload) = descriptor.matching_payload(report)?;

    decode_keyboard(descriptor, report_id, payload, events)?;
    decode_mouse(descriptor, report_id, payload, events)?;
    decode_consumer(descriptor, report_id, payload, events)?;

    Ok(())
}

fn decode_keyboard<const FIELDS: usize, const EVENTS: usize>(
    descriptor: &HidReportDescriptor<FIELDS>,
    report_id: u8,
    payload: &[u8],
    events: &mut HidReportEvents<EVENTS>,
) -> Result<(), HidReportError> {
    for field in descriptor.fields() {
        if field.report_id != report_id
            || field.is_constant()
            || field.usage_page != USAGE_PAGE_KEYBOARD
        {
            continue;
        }

        if field.is_variable() && field.bit_size == 1 {
            let mut index = 0usize;
            while index < field.count as usize {
                if matches!(field.extract_bool(payload, index), Some(true))
                    && let Some(usage) = field.usage_at(index)
                    && let Ok(usage) = u8::try_from(usage)
                {
                    events.push(HidReportEvent::KeyboardUsageDown(usage))?;
                }
                index += 1;
            }
        } else if !field.is_variable() {
            let mut index = 0usize;
            while index < field.count as usize {
                if let Some(usage) = field.extract_u32(payload, index)
                    && usage >= 4
                    && usage <= u8::MAX as u32
                {
                    events.push(HidReportEvent::KeyboardUsageDown(usage as u8))?;
                }
                index += 1;
            }
        }
    }

    Ok(())
}

fn decode_mouse<const FIELDS: usize, const EVENTS: usize>(
    descriptor: &HidReportDescriptor<FIELDS>,
    report_id: u8,
    payload: &[u8],
    events: &mut HidReportEvents<EVENTS>,
) -> Result<(), HidReportError> {
    if let Some(x) = descriptor.extract_i32(payload, report_id, USAGE_PAGE_GENERIC_DESKTOP, USAGE_X)
        && x != 0
    {
        events.push(HidReportEvent::MouseX(x))?;
    }
    if let Some(y) = descriptor.extract_i32(payload, report_id, USAGE_PAGE_GENERIC_DESKTOP, USAGE_Y)
        && y != 0
    {
        events.push(HidReportEvent::MouseY(y))?;
    }
    if let Some(wheel) =
        descriptor.extract_i32(payload, report_id, USAGE_PAGE_GENERIC_DESKTOP, USAGE_WHEEL)
        && wheel != 0
    {
        events.push(HidReportEvent::MouseWheel(wheel))?;
    }
    if let Some(pan) = descriptor.extract_i32(payload, report_id, USAGE_PAGE_CONSUMER, USAGE_AC_PAN)
        && pan != 0
    {
        events.push(HidReportEvent::MousePan(pan))?;
    }

    for (usage, button) in [
        (1, MouseButton::Left),
        (2, MouseButton::Right),
        (3, MouseButton::Middle),
        (4, MouseButton::Back),
        (5, MouseButton::Forward),
    ] {
        if matches!(
            descriptor.extract_bool(payload, report_id, USAGE_PAGE_BUTTON, usage),
            Some(true)
        ) {
            events.push(HidReportEvent::MouseButtonDown(button))?;
        }
    }

    Ok(())
}

fn decode_consumer<const FIELDS: usize, const EVENTS: usize>(
    descriptor: &HidReportDescriptor<FIELDS>,
    report_id: u8,
    payload: &[u8],
    events: &mut HidReportEvents<EVENTS>,
) -> Result<(), HidReportError> {
    for field in descriptor.fields() {
        if field.report_id != report_id
            || field.is_constant()
            || field.usage_page != USAGE_PAGE_CONSUMER
        {
            continue;
        }

        if field.is_variable() {
            let mut index = 0usize;
            while index < field.count as usize {
                if matches!(field.extract_bool(payload, index), Some(true))
                    && let Some(usage) = field.usage_at(index)
                    && usage != USAGE_AC_PAN
                {
                    events.push(HidReportEvent::ConsumerUsageDown(usage))?;
                }
                index += 1;
            }
        } else {
            let mut index = 0usize;
            while index < field.count as usize {
                if let Some(usage) = field.extract_u32(payload, index)
                    && usage != 0
                    && usage <= u16::MAX as u32
                {
                    events.push(HidReportEvent::ConsumerUsageDown(usage as u16))?;
                }
                index += 1;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb_hid::test_fixtures;

    #[test]
    fn decodes_boot_keyboard_array_report() {
        let mut descriptor = HidReportDescriptor::<4>::new(false);
        descriptor
            .push(ReportField {
                report_id: 0,
                usage_page: USAGE_PAGE_KEYBOARD,
                usage_min: 0,
                usage_max: 0xff,
                bit_offset: 16,
                bit_size: 8,
                count: 6,
                flags: 0,
                logical_min: 0,
                logical_max: 255,
            })
            .unwrap();
        let mut events = HidReportEvents::<8>::new();

        decode_report(
            &descriptor,
            &[0x02, 0x00, 0x04, 0x05, 0x00, 0x00, 0x00, 0x00],
            &mut events,
        )
        .unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![
                HidReportEvent::KeyboardUsageDown(0x04),
                HidReportEvent::KeyboardUsageDown(0x05),
            ]
        );
    }

    #[test]
    fn decodes_nkro_keyboard_variable_bits() {
        let mut descriptor = HidReportDescriptor::<2>::new(false);
        descriptor
            .push(ReportField {
                report_id: 0,
                usage_page: USAGE_PAGE_KEYBOARD,
                usage_min: 0x04,
                usage_max: 0x0b,
                bit_offset: 0,
                bit_size: 1,
                count: 8,
                flags: FIELD_FLAG_VARIABLE,
                logical_min: 0,
                logical_max: 1,
            })
            .unwrap();
        let mut events = HidReportEvents::<8>::new();

        decode_report(&descriptor, &[0b1000_0011], &mut events).unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![
                HidReportEvent::KeyboardUsageDown(0x04),
                HidReportEvent::KeyboardUsageDown(0x05),
                HidReportEvent::KeyboardUsageDown(0x0b),
            ]
        );
    }

    #[test]
    fn decodes_report_id_mouse_report() {
        let mut descriptor = HidReportDescriptor::<4>::new(true);
        descriptor
            .push(ReportField {
                report_id: 2,
                usage_page: USAGE_PAGE_BUTTON,
                usage_min: 1,
                usage_max: 3,
                bit_offset: 0,
                bit_size: 1,
                count: 3,
                flags: FIELD_FLAG_VARIABLE,
                logical_min: 0,
                logical_max: 1,
            })
            .unwrap();
        descriptor
            .push(ReportField {
                report_id: 2,
                usage_page: USAGE_PAGE_GENERIC_DESKTOP,
                usage_min: USAGE_X,
                usage_max: USAGE_Y,
                bit_offset: 8,
                bit_size: 8,
                count: 2,
                flags: FIELD_FLAG_VARIABLE,
                logical_min: -127,
                logical_max: 127,
            })
            .unwrap();
        let mut events = HidReportEvents::<8>::new();

        decode_report(&descriptor, &[2, 0b0000_0101, 0x05, 0xfe], &mut events).unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![
                HidReportEvent::MouseX(5),
                HidReportEvent::MouseY(-2),
                HidReportEvent::MouseButtonDown(MouseButton::Left),
                HidReportEvent::MouseButtonDown(MouseButton::Middle),
            ]
        );
    }

    #[test]
    fn decodes_consumer_array_report() {
        let descriptor = test_fixtures::consumer_array_descriptor();
        let mut events = HidReportEvents::<4>::new();

        decode_report(&descriptor, &[3, 0xe9, 0x00], &mut events).unwrap();

        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![HidReportEvent::ConsumerUsageDown(0x00e9)]
        );
    }

    #[test]
    fn consumer_array_with_ac_pan_range_stays_out_of_mouse_domain() {
        let mut descriptor = HidReportDescriptor::<1>::new(true);
        descriptor
            .push(ReportField {
                report_id: 3,
                usage_page: USAGE_PAGE_CONSUMER,
                usage_min: USAGE_AC_PAN,
                usage_max: USAGE_AC_PAN + 1,
                bit_offset: 0,
                bit_size: 16,
                count: 2,
                flags: 0,
                logical_min: 0,
                logical_max: i32::from(USAGE_AC_PAN + 1),
            })
            .unwrap();

        assert_eq!(
            descriptor.report_domains(3),
            HidReportDomains {
                keyboard: false,
                mouse: false,
                consumer: true,
            }
        );
    }

    #[test]
    fn golden_composite_descriptor_decodes_keyboard_mouse_pan_and_consumer_reports() {
        let descriptor = test_fixtures::composite_report_id_descriptor();
        let mut events = HidReportEvents::<8>::new();

        decode_report(
            &descriptor,
            &test_fixtures::keyboard_report(0b0000_0010, 0x04, 0x05),
            &mut events,
        )
        .unwrap();
        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![
                HidReportEvent::KeyboardUsageDown(0xe1),
                HidReportEvent::KeyboardUsageDown(0x04),
                HidReportEvent::KeyboardUsageDown(0x05),
            ]
        );

        decode_report(
            &descriptor,
            &test_fixtures::mouse_report(0b0001_0001, 12, -3, 1, -2),
            &mut events,
        )
        .unwrap();
        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![
                HidReportEvent::MouseX(12),
                HidReportEvent::MouseY(-3),
                HidReportEvent::MouseWheel(1),
                HidReportEvent::MousePan(-2),
                HidReportEvent::MouseButtonDown(MouseButton::Left),
                HidReportEvent::MouseButtonDown(MouseButton::Forward),
            ]
        );

        decode_report(
            &descriptor,
            &test_fixtures::consumer_report(0x00e9),
            &mut events,
        )
        .unwrap();
        assert_eq!(
            events.iter().collect::<Vec<_>>(),
            vec![HidReportEvent::ConsumerUsageDown(0x00e9)]
        );
    }
}
