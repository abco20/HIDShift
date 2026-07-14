use crate::ids::InterfaceId;

use super::{MAX_HID_INTERFACES, MAX_HID_REPORT_DESCRIPTOR_SIZE, MAX_HID_REPORT_SIZE};

pub const COMPOSITE_DESCRIPTOR_MAX: usize =
    MAX_HID_INTERFACES * (MAX_HID_REPORT_DESCRIPTOR_SIZE + 2);
const REPORT_ID_MAPPING_MAX: usize = u8::MAX as usize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReportIdMapping {
    pub interface_id: InterfaceId,
    pub source_report_id: Option<u8>,
    pub composite_report_id: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompositeDescriptor {
    descriptor: heapless::Vec<u8, COMPOSITE_DESCRIPTOR_MAX>,
    mappings: heapless::Vec<ReportIdMapping, REPORT_ID_MAPPING_MAX>,
    interfaces: heapless::Vec<InterfaceId, MAX_HID_INTERFACES>,
    next_report_id: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositeDescriptorError {
    InterfaceCapacity,
    DuplicateInterface,
    DescriptorTooLarge,
    CompositeDescriptorCapacity,
    MalformedDescriptor,
    InvalidReportId,
    ReportIdCapacity,
    ReportTooLarge,
    UnknownInterface,
    UnknownReportId,
    OutputBufferTooSmall,
}

impl CompositeDescriptor {
    pub const fn new() -> Self {
        Self {
            descriptor: heapless::Vec::new(),
            mappings: heapless::Vec::new(),
            interfaces: heapless::Vec::new(),
            next_report_id: 1,
        }
    }

    pub fn add_interface(
        &mut self,
        interface_id: InterfaceId,
        source_descriptor: &[u8],
    ) -> Result<(), CompositeDescriptorError> {
        if source_descriptor.len() > MAX_HID_REPORT_DESCRIPTOR_SIZE {
            return Err(CompositeDescriptorError::DescriptorTooLarge);
        }
        if self.interfaces.contains(&interface_id) {
            return Err(CompositeDescriptorError::DuplicateInterface);
        }
        let item_count = validate_and_count_report_ids(source_descriptor)?;
        let needed_mappings = item_count.max(1);
        if self.interfaces.len() == MAX_HID_INTERFACES {
            return Err(CompositeDescriptorError::InterfaceCapacity);
        }
        if self.mappings.len() + needed_mappings > REPORT_ID_MAPPING_MAX
            || self.next_report_id + needed_mappings as u16 > 256
        {
            return Err(CompositeDescriptorError::ReportIdCapacity);
        }
        let extra = if item_count == 0 { 2 } else { 0 };
        if self.descriptor.len() + source_descriptor.len() + extra > COMPOSITE_DESCRIPTOR_MAX {
            return Err(CompositeDescriptorError::CompositeDescriptorCapacity);
        }

        // Stage changes so a malformed or capacity failure never leaves a
        // partially installed HID interface.
        let descriptor_start = self.descriptor.len();
        let mappings_start = self.mappings.len();
        let next_start = self.next_report_id;
        let result = if item_count == 0 {
            let assigned = self.allocate_report_id(interface_id, None)?;
            self.descriptor
                .extend_from_slice(&[0x85, assigned])
                .map_err(|_| CompositeDescriptorError::CompositeDescriptorCapacity)?;
            self.descriptor
                .extend_from_slice(source_descriptor)
                .map_err(|_| CompositeDescriptorError::CompositeDescriptorCapacity)
        } else {
            self.copy_and_remap_descriptor(interface_id, source_descriptor)
        };
        if let Err(error) = result {
            self.descriptor.truncate(descriptor_start);
            self.mappings.truncate(mappings_start);
            self.next_report_id = next_start;
            return Err(error);
        }
        self.interfaces
            .push(interface_id)
            .map_err(|_| CompositeDescriptorError::InterfaceCapacity)?;
        Ok(())
    }

    pub fn descriptor(&self) -> &[u8] {
        self.descriptor.as_slice()
    }

    pub fn mappings(&self) -> &[ReportIdMapping] {
        self.mappings.as_slice()
    }

    pub fn interface_count(&self) -> usize {
        self.interfaces.len()
    }

    pub fn encode_input_report(
        &self,
        interface_id: InterfaceId,
        source_report: &[u8],
        out: &mut [u8],
    ) -> Result<usize, CompositeDescriptorError> {
        if source_report.len() > MAX_HID_REPORT_SIZE {
            return Err(CompositeDescriptorError::ReportTooLarge);
        }
        let mut interface_mappings = self
            .mappings
            .iter()
            .filter(|mapping| mapping.interface_id == interface_id);
        let Some(first) = interface_mappings.clone().next() else {
            return Err(CompositeDescriptorError::UnknownInterface);
        };
        match first.source_report_id {
            None => {
                if out.len() < source_report.len() + 1 {
                    return Err(CompositeDescriptorError::OutputBufferTooSmall);
                }
                out[0] = first.composite_report_id;
                out[1..source_report.len() + 1].copy_from_slice(source_report);
                Ok(source_report.len() + 1)
            }
            Some(_) => {
                let source_id = *source_report
                    .first()
                    .ok_or(CompositeDescriptorError::UnknownReportId)?;
                let mapping = interface_mappings
                    .find(|mapping| mapping.source_report_id == Some(source_id))
                    .ok_or(CompositeDescriptorError::UnknownReportId)?;
                if out.len() < source_report.len() {
                    return Err(CompositeDescriptorError::OutputBufferTooSmall);
                }
                out[..source_report.len()].copy_from_slice(source_report);
                out[0] = mapping.composite_report_id;
                Ok(source_report.len())
            }
        }
    }

    pub fn decode_host_report(
        &self,
        composite_report: &[u8],
        out: &mut [u8],
    ) -> Result<(InterfaceId, Option<u8>, usize), CompositeDescriptorError> {
        let composite_id = *composite_report
            .first()
            .ok_or(CompositeDescriptorError::UnknownReportId)?;
        let mapping = self
            .mappings
            .iter()
            .find(|mapping| mapping.composite_report_id == composite_id)
            .ok_or(CompositeDescriptorError::UnknownReportId)?;
        match mapping.source_report_id {
            None => {
                let source = &composite_report[1..];
                if out.len() < source.len() {
                    return Err(CompositeDescriptorError::OutputBufferTooSmall);
                }
                out[..source.len()].copy_from_slice(source);
                Ok((mapping.interface_id, None, source.len()))
            }
            Some(source_id) => {
                if out.len() < composite_report.len() {
                    return Err(CompositeDescriptorError::OutputBufferTooSmall);
                }
                out[..composite_report.len()].copy_from_slice(composite_report);
                out[0] = source_id;
                Ok((
                    mapping.interface_id,
                    Some(source_id),
                    composite_report.len(),
                ))
            }
        }
    }

    fn allocate_report_id(
        &mut self,
        interface_id: InterfaceId,
        source_report_id: Option<u8>,
    ) -> Result<u8, CompositeDescriptorError> {
        if source_report_id == Some(0) || self.next_report_id > u8::MAX as u16 {
            return Err(CompositeDescriptorError::InvalidReportId);
        }
        if self.mappings.iter().any(|mapping| {
            mapping.interface_id == interface_id && mapping.source_report_id == source_report_id
        }) {
            return Err(CompositeDescriptorError::InvalidReportId);
        }
        let composite_report_id = self.next_report_id as u8;
        self.next_report_id += 1;
        self.mappings
            .push(ReportIdMapping {
                interface_id,
                source_report_id,
                composite_report_id,
            })
            .map_err(|_| CompositeDescriptorError::ReportIdCapacity)?;
        Ok(composite_report_id)
    }

    fn copy_and_remap_descriptor(
        &mut self,
        interface_id: InterfaceId,
        source: &[u8],
    ) -> Result<(), CompositeDescriptorError> {
        let mut offset = 0;
        while offset < source.len() {
            let item_len = hid_item_len(source, offset)?;
            let prefix = source[offset];
            self.descriptor
                .push(prefix)
                .map_err(|_| CompositeDescriptorError::CompositeDescriptorCapacity)?;
            if is_report_id_item(prefix) {
                let source_id = source[offset + 1];
                let mapped = if let Some(existing) = self.mappings.iter().find(|mapping| {
                    mapping.interface_id == interface_id
                        && mapping.source_report_id == Some(source_id)
                }) {
                    existing.composite_report_id
                } else {
                    self.allocate_report_id(interface_id, Some(source_id))?
                };
                self.descriptor
                    .push(mapped)
                    .map_err(|_| CompositeDescriptorError::CompositeDescriptorCapacity)?;
            } else {
                self.descriptor
                    .extend_from_slice(&source[offset + 1..offset + item_len])
                    .map_err(|_| CompositeDescriptorError::CompositeDescriptorCapacity)?;
            }
            offset += item_len;
        }
        Ok(())
    }
}

impl Default for CompositeDescriptor {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_and_count_report_ids(source: &[u8]) -> Result<usize, CompositeDescriptorError> {
    let mut offset = 0;
    let mut ids = heapless::Vec::<u8, REPORT_ID_MAPPING_MAX>::new();
    while offset < source.len() {
        let len = hid_item_len(source, offset)?;
        if is_report_id_item(source[offset]) {
            let id = source[offset + 1];
            if id == 0 {
                return Err(CompositeDescriptorError::InvalidReportId);
            }
            if !ids.contains(&id) {
                ids.push(id)
                    .map_err(|_| CompositeDescriptorError::ReportIdCapacity)?;
            }
        }
        offset += len;
    }
    Ok(ids.len())
}

fn hid_item_len(source: &[u8], offset: usize) -> Result<usize, CompositeDescriptorError> {
    let prefix = *source
        .get(offset)
        .ok_or(CompositeDescriptorError::MalformedDescriptor)?;
    let len = if prefix == 0xfe {
        let payload_len = *source
            .get(offset + 1)
            .ok_or(CompositeDescriptorError::MalformedDescriptor)?
            as usize;
        3 + payload_len
    } else {
        1 + match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        }
    };
    if offset + len > source.len() {
        return Err(CompositeDescriptorError::MalformedDescriptor);
    }
    Ok(len)
}

const fn is_report_id_item(prefix: u8) -> bool {
    // Short item: Global type (1), Report ID tag (8), one-byte payload.
    prefix == 0x85
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal keyboard-like descriptor without a Report ID.
    const KEYBOARD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x08, 0x95, 0x08, 0x81, 0x02, 0xc0,
    ];
    const VENDOR_WITH_IDS: &[u8] = &[
        0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x07, 0x75, 0x08, 0x95, 0x03, 0x81, 0x02,
        0x85, 0x09, 0x95, 0x02, 0x91, 0x02, 0xc0,
    ];

    #[test]
    fn interfaces_without_ids_get_unique_prefixes() {
        let mut composite = CompositeDescriptor::new();
        composite.add_interface(InterfaceId(1), KEYBOARD).unwrap();
        composite.add_interface(InterfaceId(2), KEYBOARD).unwrap();
        assert_eq!(&composite.descriptor()[..2], &[0x85, 1]);
        assert_eq!(
            &composite.descriptor()[KEYBOARD.len() + 2..KEYBOARD.len() + 4],
            &[0x85, 2]
        );

        let mut report = [0; MAX_HID_REPORT_SIZE + 1];
        assert_eq!(
            composite.encode_input_report(InterfaceId(2), &[0, 4, 0], &mut report),
            Ok(4)
        );
        assert_eq!(&report[..4], &[2, 0, 4, 0]);
    }

    #[test]
    fn existing_ids_are_remapped_and_bidirectional_reports_are_reversible() {
        let mut composite = CompositeDescriptor::new();
        composite
            .add_interface(InterfaceId(4), VENDOR_WITH_IDS)
            .unwrap();
        assert_eq!(composite.mappings().len(), 2);
        assert_eq!(composite.mappings()[0].source_report_id, Some(7));
        assert_eq!(composite.mappings()[1].source_report_id, Some(9));
        assert!(
            composite
                .descriptor()
                .windows(2)
                .any(|item| item == [0x85, 1])
        );
        assert!(
            composite
                .descriptor()
                .windows(2)
                .any(|item| item == [0x85, 2])
        );

        let mut wire = [0; MAX_HID_REPORT_SIZE + 1];
        let len = composite
            .encode_input_report(InterfaceId(4), &[7, 0xaa, 0xbb], &mut wire)
            .unwrap();
        assert_eq!(&wire[..len], &[1, 0xaa, 0xbb]);

        let mut source = [0; MAX_HID_REPORT_SIZE];
        let (interface, id, len) = composite
            .decode_host_report(&[2, 0x10, 0x20], &mut source)
            .unwrap();
        assert_eq!((interface, id), (InterfaceId(4), Some(9)));
        assert_eq!(&source[..len], &[9, 0x10, 0x20]);
    }

    #[test]
    fn malformed_descriptor_does_not_partially_mutate_composite() {
        let mut composite = CompositeDescriptor::new();
        composite.add_interface(InterfaceId(1), KEYBOARD).unwrap();
        let before = composite.clone();
        assert_eq!(
            composite.add_interface(InterfaceId(2), &[0x75]),
            Err(CompositeDescriptorError::MalformedDescriptor)
        );
        assert_eq!(composite, before);
    }
}
