use crate::ids::{DeviceId, InterfaceId};

pub const USB_TOPOLOGY_DEVICES_MAX: usize = 4;
pub const USB_TOPOLOGY_INTERFACES_MAX: usize = 8;

pub type DefaultUsbTopologyManager =
    UsbTopologyManager<USB_TOPOLOGY_DEVICES_MAX, USB_TOPOLOGY_INTERFACES_MAX>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbDeviceRoute {
    Direct,
    Downstream { hub_device_id: DeviceId, port: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbDeviceTopologyEntry {
    pub device_id: DeviceId,
    pub usb_address: u8,
    pub route: UsbDeviceRoute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbInterfaceTopologyEntry {
    pub interface_id: InterfaceId,
    pub device_id: DeviceId,
    pub interface_number: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsbTopologyError {
    DeviceCapacity,
    InterfaceCapacity,
    UnknownDevice,
    OccupiedRoute,
    HubParentMissing,
    HubDepthExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbTopologyRemoval<const DEVICES: usize, const INTERFACES: usize> {
    device_count: usize,
    interface_count: usize,
    devices: [Option<UsbDeviceTopologyEntry>; DEVICES],
    interfaces: [Option<UsbInterfaceTopologyEntry>; INTERFACES],
}

impl<const DEVICES: usize, const INTERFACES: usize> UsbTopologyRemoval<DEVICES, INTERFACES> {
    const fn new() -> Self {
        Self {
            device_count: 0,
            interface_count: 0,
            devices: [None; DEVICES],
            interfaces: [None; INTERFACES],
        }
    }

    pub fn devices(&self) -> impl Iterator<Item = UsbDeviceTopologyEntry> + '_ {
        self.devices[..self.device_count]
            .iter()
            .filter_map(|entry| *entry)
    }

    pub fn interfaces(&self) -> impl Iterator<Item = UsbInterfaceTopologyEntry> + '_ {
        self.interfaces[..self.interface_count]
            .iter()
            .filter_map(|entry| *entry)
    }

    fn push_device(&mut self, entry: UsbDeviceTopologyEntry) {
        self.devices[self.device_count] = Some(entry);
        self.device_count += 1;
    }

    fn push_interface(&mut self, entry: UsbInterfaceTopologyEntry) {
        self.interfaces[self.interface_count] = Some(entry);
        self.interface_count += 1;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UsbTopologyManager<const DEVICES: usize, const INTERFACES: usize> {
    device_count: usize,
    interface_count: usize,
    devices: [Option<UsbDeviceTopologyEntry>; DEVICES],
    interfaces: [Option<UsbInterfaceTopologyEntry>; INTERFACES],
}

impl<const DEVICES: usize, const INTERFACES: usize> UsbTopologyManager<DEVICES, INTERFACES> {
    pub const fn new() -> Self {
        Self {
            device_count: 0,
            interface_count: 0,
            devices: [None; DEVICES],
            interfaces: [None; INTERFACES],
        }
    }

    pub const fn device_count(&self) -> usize {
        self.device_count
    }

    pub const fn interface_count(&self) -> usize {
        self.interface_count
    }

    pub fn devices(&self) -> impl Iterator<Item = UsbDeviceTopologyEntry> + '_ {
        self.devices[..self.device_count]
            .iter()
            .filter_map(|entry| *entry)
    }

    pub fn interfaces(&self) -> impl Iterator<Item = UsbInterfaceTopologyEntry> + '_ {
        self.interfaces[..self.interface_count]
            .iter()
            .filter_map(|entry| *entry)
    }

    pub fn connect_device(
        &mut self,
        usb_address: u8,
        route: UsbDeviceRoute,
    ) -> Result<DeviceId, UsbTopologyError> {
        self.validate_route(route)?;

        if self.devices().any(|entry| entry.route == route) {
            return Err(UsbTopologyError::OccupiedRoute);
        }

        let device_id = self.alloc_device_id()?;
        self.devices[self.device_count] = Some(UsbDeviceTopologyEntry {
            device_id,
            usb_address,
            route,
        });
        self.device_count += 1;
        Ok(device_id)
    }

    pub fn register_interface(
        &mut self,
        device_id: DeviceId,
        interface_number: u8,
    ) -> Result<InterfaceId, UsbTopologyError> {
        if self.device(device_id).is_none() {
            return Err(UsbTopologyError::UnknownDevice);
        }

        if let Some(entry) = self.interfaces().find(|entry| {
            entry.device_id == device_id && entry.interface_number == interface_number
        }) {
            return Ok(entry.interface_id);
        }

        let interface_id = self.alloc_interface_id()?;
        self.interfaces[self.interface_count] = Some(UsbInterfaceTopologyEntry {
            interface_id,
            device_id,
            interface_number,
        });
        self.interface_count += 1;
        Ok(interface_id)
    }

    pub fn device(&self, device_id: DeviceId) -> Option<UsbDeviceTopologyEntry> {
        self.devices().find(|entry| entry.device_id == device_id)
    }

    pub fn remove_device(
        &mut self,
        device_id: DeviceId,
    ) -> Result<UsbTopologyRemoval<DEVICES, INTERFACES>, UsbTopologyError> {
        if self.device(device_id).is_none() {
            return Err(UsbTopologyError::UnknownDevice);
        }

        let mut removal = UsbTopologyRemoval::new();
        while let Some(next_device) = self.next_removable_device(device_id, &removal) {
            self.remove_interfaces_for_device(next_device, &mut removal);
            self.remove_device_entry(next_device, &mut removal);
        }
        Ok(removal)
    }

    fn validate_route(&self, route: UsbDeviceRoute) -> Result<(), UsbTopologyError> {
        match route {
            UsbDeviceRoute::Direct => Ok(()),
            UsbDeviceRoute::Downstream {
                hub_device_id,
                port: _,
            } => {
                let Some(parent) = self.device(hub_device_id) else {
                    return Err(UsbTopologyError::HubParentMissing);
                };
                match parent.route {
                    UsbDeviceRoute::Direct => Ok(()),
                    UsbDeviceRoute::Downstream { .. } => Err(UsbTopologyError::HubDepthExceeded),
                }
            }
        }
    }

    fn alloc_device_id(&self) -> Result<DeviceId, UsbTopologyError> {
        for value in 1..=DEVICES {
            let candidate = DeviceId(value as u8);
            if self.device(candidate).is_none() {
                return Ok(candidate);
            }
        }
        Err(UsbTopologyError::DeviceCapacity)
    }

    fn alloc_interface_id(&self) -> Result<InterfaceId, UsbTopologyError> {
        for value in 1..=INTERFACES {
            let candidate = InterfaceId(value as u8);
            if !self
                .interfaces()
                .any(|entry| entry.interface_id == candidate)
            {
                return Ok(candidate);
            }
        }
        Err(UsbTopologyError::InterfaceCapacity)
    }

    fn next_removable_device(
        &self,
        root_device_id: DeviceId,
        removal: &UsbTopologyRemoval<DEVICES, INTERFACES>,
    ) -> Option<DeviceId> {
        self.devices()
            .find(|entry| match entry.route {
                _ if entry.device_id == root_device_id => true,
                UsbDeviceRoute::Direct => false,
                UsbDeviceRoute::Downstream { hub_device_id, .. } => {
                    hub_device_id == root_device_id
                        || removal
                            .devices()
                            .any(|removed| removed.device_id == hub_device_id)
                }
            })
            .map(|entry| entry.device_id)
    }

    fn remove_interfaces_for_device(
        &mut self,
        device_id: DeviceId,
        removal: &mut UsbTopologyRemoval<DEVICES, INTERFACES>,
    ) {
        let mut index = 0usize;
        while index < self.interface_count {
            let Some(entry) = self.interfaces[index] else {
                index += 1;
                continue;
            };
            if entry.device_id == device_id {
                removal.push_interface(entry);
                self.interfaces.swap(index, self.interface_count - 1);
                self.interfaces[self.interface_count - 1] = None;
                self.interface_count -= 1;
            } else {
                index += 1;
            }
        }
    }

    fn remove_device_entry(
        &mut self,
        device_id: DeviceId,
        removal: &mut UsbTopologyRemoval<DEVICES, INTERFACES>,
    ) {
        let mut index = 0usize;
        while index < self.device_count {
            let Some(entry) = self.devices[index] else {
                index += 1;
                continue;
            };
            if entry.device_id == device_id {
                removal.push_device(entry);
                self.devices.swap(index, self.device_count - 1);
                self.devices[self.device_count - 1] = None;
                self.device_count -= 1;
                return;
            }
            index += 1;
        }
    }
}

impl<const DEVICES: usize, const INTERFACES: usize> Default
    for UsbTopologyManager<DEVICES, INTERFACES>
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_ids_are_unique_across_devices() {
        let mut topology = UsbTopologyManager::<4, 8>::new();
        let device_a = topology.connect_device(3, UsbDeviceRoute::Direct).unwrap();
        let device_b = topology
            .connect_device(
                4,
                UsbDeviceRoute::Downstream {
                    hub_device_id: device_a,
                    port: 1,
                },
            )
            .unwrap();

        let interface_a = topology.register_interface(device_a, 1).unwrap();
        let interface_b = topology.register_interface(device_b, 1).unwrap();

        assert_ne!(interface_a, interface_b);
        assert_eq!(topology.interface_count(), 2);
    }

    #[test]
    fn removing_hub_cascades_to_downstream_device_interfaces() {
        let mut topology = UsbTopologyManager::<4, 8>::new();
        let hub = topology.connect_device(1, UsbDeviceRoute::Direct).unwrap();
        let child = topology
            .connect_device(
                2,
                UsbDeviceRoute::Downstream {
                    hub_device_id: hub,
                    port: 3,
                },
            )
            .unwrap();
        let hub_interface = topology.register_interface(hub, 0).unwrap();
        let child_interface = topology.register_interface(child, 1).unwrap();

        let removal = topology.remove_device(hub).unwrap();
        let mut removed_devices = heapless::Vec::<DeviceId, 4>::new();
        for device_id in removal.devices().map(|entry| entry.device_id) {
            removed_devices.push(device_id).unwrap();
        }
        let mut removed_interfaces = heapless::Vec::<InterfaceId, 8>::new();
        for interface_id in removal.interfaces().map(|entry| entry.interface_id) {
            removed_interfaces.push(interface_id).unwrap();
        }

        assert_eq!(
            removed_devices,
            heapless::Vec::<DeviceId, 4>::from_slice(&[hub, child]).unwrap()
        );
        assert_eq!(
            removed_interfaces,
            heapless::Vec::<InterfaceId, 8>::from_slice(&[hub_interface, child_interface]).unwrap()
        );
        assert_eq!(topology.device_count(), 0);
        assert_eq!(topology.interface_count(), 0);
    }

    #[test]
    fn removing_downstream_leaf_releases_its_route() {
        let mut topology = UsbTopologyManager::<4, 8>::new();
        let hub = topology.connect_device(1, UsbDeviceRoute::Direct).unwrap();
        let child = topology
            .connect_device(
                2,
                UsbDeviceRoute::Downstream {
                    hub_device_id: hub,
                    port: 1,
                },
            )
            .unwrap();
        let child_interface = topology.register_interface(child, 1).unwrap();

        let removal = topology.remove_device(child).unwrap();
        let mut removed_devices = heapless::Vec::<DeviceId, 4>::new();
        for device_id in removal.devices().map(|entry| entry.device_id) {
            removed_devices.push(device_id).unwrap();
        }
        let mut removed_interfaces = heapless::Vec::<InterfaceId, 8>::new();
        for interface_id in removal.interfaces().map(|entry| entry.interface_id) {
            removed_interfaces.push(interface_id).unwrap();
        }

        assert_eq!(
            removed_devices,
            heapless::Vec::<DeviceId, 4>::from_slice(&[child]).unwrap()
        );
        assert_eq!(
            removed_interfaces,
            heapless::Vec::<InterfaceId, 8>::from_slice(&[child_interface]).unwrap()
        );
        assert_eq!(topology.device_count(), 1);
        assert_eq!(topology.interface_count(), 0);
        assert_eq!(
            topology.connect_device(
                3,
                UsbDeviceRoute::Downstream {
                    hub_device_id: hub,
                    port: 1,
                },
            ),
            Ok(child)
        );
    }

    #[test]
    fn rejects_second_level_hub_depth() {
        let mut topology = UsbTopologyManager::<4, 8>::new();
        let hub = topology.connect_device(1, UsbDeviceRoute::Direct).unwrap();
        let child_hub = topology
            .connect_device(
                2,
                UsbDeviceRoute::Downstream {
                    hub_device_id: hub,
                    port: 2,
                },
            )
            .unwrap();

        assert_eq!(
            topology.connect_device(
                3,
                UsbDeviceRoute::Downstream {
                    hub_device_id: child_hub,
                    port: 1,
                },
            ),
            Err(UsbTopologyError::HubDepthExceeded)
        );
    }

    #[test]
    fn enforces_device_and_interface_caps() {
        let mut topology = UsbTopologyManager::<2, 3>::new();
        let device_a = topology.connect_device(1, UsbDeviceRoute::Direct).unwrap();
        let device_b = topology
            .connect_device(
                2,
                UsbDeviceRoute::Downstream {
                    hub_device_id: device_a,
                    port: 1,
                },
            )
            .unwrap();

        assert_eq!(
            topology.connect_device(
                3,
                UsbDeviceRoute::Downstream {
                    hub_device_id: device_a,
                    port: 2,
                },
            ),
            Err(UsbTopologyError::DeviceCapacity)
        );

        topology.register_interface(device_a, 0).unwrap();
        topology.register_interface(device_a, 1).unwrap();
        topology.register_interface(device_b, 0).unwrap();
        assert_eq!(
            topology.register_interface(device_b, 1),
            Err(UsbTopologyError::InterfaceCapacity)
        );
    }
}
