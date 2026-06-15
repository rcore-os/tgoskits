// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Device registry and first-stage bus routing tables.

use alloc::{rc::Rc, vec::Vec};

use axdevice_base::{DeviceAddrRange, PortRange, SysRegAddrRange};
use axvm_types::GuestPhysAddrRange;

use crate::{
    bus::{BusAccess, BusAddress, BusResponse},
    model::{DeviceError, DeviceId, DeviceOps, DeviceResult},
    resource::Resource,
};

/// A route entry for MMIO accesses.
type MmioRoute = (GuestPhysAddrRange, DeviceId);
/// A route entry for port I/O accesses.
type PioRoute = (PortRange, DeviceId);
/// A route entry for system register accesses.
type SysRegRoute = (SysRegAddrRange, DeviceId);

/// Registry for emulated devices and their bus-visible resources.
#[derive(Default)]
pub struct DeviceRegistry {
    devices: Vec<Rc<dyn DeviceOps>>,
    mmio_routes: Vec<MmioRoute>,
    pio_routes: Vec<PioRoute>,
    sysreg_routes: Vec<SysRegRoute>,
}

impl DeviceRegistry {
    /// Creates an empty registry.
    pub const fn new() -> Self {
        Self {
            devices: Vec::new(),
            mmio_routes: Vec::new(),
            pio_routes: Vec::new(),
            sysreg_routes: Vec::new(),
        }
    }

    /// Registers a device and indexes its bus resources.
    pub fn register_device(&mut self, device: Rc<dyn DeviceOps>) -> DeviceResult<DeviceId> {
        let id = device.id();
        if self.devices.iter().any(|existing| existing.id() == id) {
            return Err(DeviceError::DuplicateDeviceId { id });
        }

        self.check_resource_conflicts(device.resources())?;

        for resource in device.resources() {
            match *resource {
                Resource::Mmio(range) => self.mmio_routes.push((range, id)),
                Resource::Pio(range) => self.pio_routes.push((range, id)),
                Resource::SysReg(range) => self.sysreg_routes.push((range, id)),
                Resource::Irq(_)
                | Resource::Msi { .. }
                | Resource::Dma
                | Resource::PciBar { .. } => {}
            }
        }

        self.devices.push(device);
        Ok(id)
    }

    /// Finds a device by identifier.
    pub fn find_device(&self, id: DeviceId) -> Option<Rc<dyn DeviceOps>> {
        self.devices
            .iter()
            .find(|device| device.id() == id)
            .cloned()
    }

    /// Dispatches a normalized bus access to the registered device that owns the route.
    pub fn dispatch(&self, access: BusAccess) -> DeviceResult<BusResponse> {
        if access.kind != access.addr.kind() {
            return Err(DeviceError::BusAddressMismatch {
                kind: access.kind,
                address: access.addr,
            });
        }

        let device_id = match access.addr {
            BusAddress::Mmio(addr) => self
                .mmio_routes
                .iter()
                .find(|(range, _)| range.contains(addr))
                .map(|(_, id)| *id),
            BusAddress::Pio(port) => self
                .pio_routes
                .iter()
                .find(|(range, _)| range.contains(port))
                .map(|(_, id)| *id),
            BusAddress::SysReg(addr) => self
                .sysreg_routes
                .iter()
                .find(|(range, _)| range.contains(addr))
                .map(|(_, id)| *id),
        }
        .ok_or(DeviceError::DeviceNotFound {
            kind: access.kind,
            address: access.addr,
        })?;

        let device = self
            .find_device(device_id)
            .ok_or(DeviceError::DeviceNotFound {
                kind: access.kind,
                address: access.addr,
            })?;

        device.access(access)
    }

    /// Returns the number of registered devices.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Returns the number of MMIO routes.
    pub fn mmio_route_count(&self) -> usize {
        self.mmio_routes.len()
    }

    /// Returns the number of port I/O routes.
    pub fn pio_route_count(&self) -> usize {
        self.pio_routes.len()
    }

    /// Returns the number of system register routes.
    pub fn sysreg_route_count(&self) -> usize {
        self.sysreg_routes.len()
    }

    fn check_resource_conflicts(&self, resources: &[Resource]) -> DeviceResult {
        for resource in resources {
            match *resource {
                Resource::Mmio(requested) => {
                    if let Some((existing, _)) = self
                        .mmio_routes
                        .iter()
                        .find(|(existing, _)| mmio_ranges_overlap(*existing, requested))
                    {
                        return Err(DeviceError::ResourceConflict {
                            existing: Resource::Mmio(*existing),
                            requested: *resource,
                        });
                    }
                }
                Resource::Pio(requested) => {
                    if let Some((existing, _)) = self
                        .pio_routes
                        .iter()
                        .find(|(existing, _)| port_ranges_overlap(*existing, requested))
                    {
                        return Err(DeviceError::ResourceConflict {
                            existing: Resource::Pio(*existing),
                            requested: *resource,
                        });
                    }
                }
                Resource::SysReg(requested) => {
                    if let Some((existing, _)) = self
                        .sysreg_routes
                        .iter()
                        .find(|(existing, _)| sysreg_ranges_overlap(*existing, requested))
                    {
                        return Err(DeviceError::ResourceConflict {
                            existing: Resource::SysReg(*existing),
                            requested: *resource,
                        });
                    }
                }
                Resource::Irq(_)
                | Resource::Msi { .. }
                | Resource::Dma
                | Resource::PciBar { .. } => {}
            }
        }
        Ok(())
    }
}

#[inline]
fn mmio_ranges_overlap(left: GuestPhysAddrRange, right: GuestPhysAddrRange) -> bool {
    left.start < right.end && right.start < left.end
}

#[inline]
fn port_ranges_overlap(left: PortRange, right: PortRange) -> bool {
    left.start <= right.end && right.start <= left.end
}

#[inline]
fn sysreg_ranges_overlap(left: SysRegAddrRange, right: SysRegAddrRange) -> bool {
    left.start <= right.end && right.start <= left.end
}
