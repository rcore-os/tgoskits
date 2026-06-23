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

//! Adapters that wrap the old [`BaseDeviceOps`](crate::BaseDeviceOps)
//! implementations so they can be registered into an `AxVmDevices` that
//! expects the new [`Device`](crate::Device) trait.
//!
//! These adapters are intended as a migration aid.  Once each device is
//! rewritten to implement `Device` natively the corresponding adapter can
//! be removed.

use alloc::{boxed::Box, string::String, sync::Arc};
use core::any::Any;

use crate::{
    BaseDeviceOps, Device, EmuDeviceType, GuestPhysAddr, Resource,
    device::{BusAccess, BusResponse, DeviceError, Port, PortRange, SysRegAddr, SysRegAddrRange},
};

fn type_name(emu_type: EmuDeviceType) -> String {
    alloc::format!("{:?}-adapter", emu_type)
}

fn mmio_resources(range: &crate::GuestPhysAddrRange) -> Box<[Resource]> {
    let base = range.start.as_usize() as u64;
    let size = if range.end.as_usize() >= range.start.as_usize() {
        (range.end.as_usize() - range.start.as_usize()) as u64
    } else {
        0
    };
    alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice()
}

fn sysreg_resources(range: &SysRegAddrRange) -> Box<[Resource]> {
    let addr = range.start.0 as u32;
    let count = if range.end.0 >= range.start.0 {
        (range.end.0 - range.start.0) as u32 + 1
    } else {
        0
    };
    alloc::vec![Resource::SysReg { addr, count }].into_boxed_slice()
}

fn port_resources(range: &PortRange) -> Box<[Resource]> {
    let base = range.start.0;
    let size = if range.end.0 >= range.start.0 {
        (range.end.0 - range.start.0).wrapping_add(1)
    } else {
        0
    };
    alloc::vec![Resource::PortRange { base, size }].into_boxed_slice()
}

// ---------------------------------------------------------------------------
// MmioDeviceAdapter
// ---------------------------------------------------------------------------

/// Wraps an old-style [`BaseDeviceOps<GuestPhysAddrRange>`] device so that it
/// implements the new [`Device`] trait.
pub struct MmioDeviceAdapter<T> {
    /// The inner device wrapped in an `Arc`.
    inner: Arc<T>,
    /// The human-readable name of this adapter.
    name: String,
    /// Cached resource snapshot.
    resources: Box<[Resource]>,
}

impl<T: Send> MmioDeviceAdapter<T>
where
    T: BaseDeviceOps<crate::GuestPhysAddrRange>,
{
    /// Creates a new `MmioDeviceAdapter` from an owned device.
    pub fn new(device: T) -> Self {
        let resources = mmio_resources(&device.address_range());
        Self {
            name: type_name(device.emu_type()),
            inner: Arc::new(device),
            resources,
        }
    }

    /// Creates an `Arc<dyn Device>` from an existing `Arc<T>`.
    pub fn from_arc(device: Arc<T>) -> Arc<dyn Device>
    where
        T: Send + Sync + 'static,
        T: BaseDeviceOps<crate::GuestPhysAddrRange>,
    {
        let resources = mmio_resources(&device.address_range());
        Arc::new(Self {
            name: type_name(device.emu_type()),
            inner: device,
            resources,
        })
    }

    /// Returns a reference to the inner device.
    pub fn inner(&self) -> &T {
        &self.inner
    }
}

// SAFETY: The inner device uses internal synchronisation (e.g. `Mutex`,
// `UnsafeCell` with proper barriers) and has been safely shared across
// threads in the existing codebase via `Arc`.
// The bounds match what the concrete device types satisfy.
unsafe impl<T: Send + Sync> Send for MmioDeviceAdapter<T> {}
unsafe impl<T: Send + Sync> Sync for MmioDeviceAdapter<T> {}

impl<T: Send + Sync + 'static> Device for MmioDeviceAdapter<T>
where
    T: BaseDeviceOps<crate::GuestPhysAddrRange>,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let addr = GuestPhysAddr::from(access.addr as usize);
        if access.is_read {
            self.inner
                .handle_read(addr, access.width)
                .map(|v| BusResponse::Read { value: v as u64 })
                .map_err(|_| DeviceError::Internal)
        } else {
            self.inner
                .handle_write(addr, access.width, access.data as usize)
                .map(|_| BusResponse::Write)
                .map_err(|_| DeviceError::Internal)
        }
    }

    fn as_any(&self) -> &dyn Any {
        &*self.inner
    }
}

// ---------------------------------------------------------------------------
// SysRegDeviceAdapter
// ---------------------------------------------------------------------------

/// Wraps an old-style [`BaseDeviceOps<SysRegAddrRange>`] device so that it
/// implements the new [`Device`](crate::Device) trait.
pub struct SysRegDeviceAdapter<T> {
    /// The inner device wrapped in an `Arc`.
    inner: Arc<T>,
    /// The human-readable name of this adapter.
    name: String,
    /// Cached resource snapshot.
    resources: Box<[Resource]>,
}

impl<T: Send> SysRegDeviceAdapter<T>
where
    T: BaseDeviceOps<SysRegAddrRange>,
{
    /// Creates a new `SysRegDeviceAdapter` from an owned device.
    pub fn new(device: T) -> Self {
        let resources = sysreg_resources(&device.address_range());
        Self {
            name: type_name(device.emu_type()),
            inner: Arc::new(device),
            resources,
        }
    }

    /// Creates an `Arc<dyn Device>` from an existing `Arc<T>`.
    pub fn from_arc(device: Arc<T>) -> Arc<dyn Device>
    where
        T: Send + Sync + 'static,
        T: BaseDeviceOps<SysRegAddrRange>,
    {
        let resources = sysreg_resources(&device.address_range());
        Arc::new(Self {
            name: type_name(device.emu_type()),
            inner: device,
            resources,
        })
    }

    /// Returns a reference to the inner device.
    pub fn inner(&self) -> &T {
        &self.inner
    }
}

unsafe impl<T: Send + Sync> Send for SysRegDeviceAdapter<T> {}
unsafe impl<T: Send + Sync> Sync for SysRegDeviceAdapter<T> {}

impl<T: Send + Sync + 'static> Device for SysRegDeviceAdapter<T>
where
    T: BaseDeviceOps<SysRegAddrRange>,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let addr = SysRegAddr::new(access.addr as usize);
        if access.is_read {
            self.inner
                .handle_read(addr, access.width)
                .map(|v| BusResponse::Read { value: v as u64 })
                .map_err(|_| DeviceError::Internal)
        } else {
            self.inner
                .handle_write(addr, access.width, access.data as usize)
                .map(|_| BusResponse::Write)
                .map_err(|_| DeviceError::Internal)
        }
    }

    fn as_any(&self) -> &dyn Any {
        &*self.inner
    }
}

// ---------------------------------------------------------------------------
// PortDeviceAdapter
// ---------------------------------------------------------------------------

/// Wraps an old-style [`BaseDeviceOps<PortRange>`] device so that it implements
/// the new [`Device`](crate::Device) trait.
pub struct PortDeviceAdapter<T> {
    /// The inner device wrapped in an `Arc`.
    inner: Arc<T>,
    /// The human-readable name of this adapter.
    name: String,
    /// Cached resource snapshot.
    resources: Box<[Resource]>,
}

impl<T: Send> PortDeviceAdapter<T>
where
    T: BaseDeviceOps<PortRange>,
{
    /// Creates a new `PortDeviceAdapter` from an owned device.
    pub fn new(device: T) -> Self {
        let resources = port_resources(&device.address_range());
        Self {
            name: type_name(device.emu_type()),
            inner: Arc::new(device),
            resources,
        }
    }

    /// Creates an `Arc<dyn Device>` from an existing `Arc<T>`.
    pub fn from_arc(device: Arc<T>) -> Arc<dyn Device>
    where
        T: Send + Sync + 'static,
        T: BaseDeviceOps<PortRange>,
    {
        let resources = port_resources(&device.address_range());
        Arc::new(Self {
            name: type_name(device.emu_type()),
            inner: device,
            resources,
        })
    }

    /// Returns a reference to the inner device.
    pub fn inner(&self) -> &T {
        &self.inner
    }
}

unsafe impl<T: Send + Sync> Send for PortDeviceAdapter<T> {}
unsafe impl<T: Send + Sync> Sync for PortDeviceAdapter<T> {}

impl<T: Send + Sync + 'static> Device for PortDeviceAdapter<T>
where
    T: BaseDeviceOps<PortRange>,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> Result<BusResponse, DeviceError> {
        let port = Port::new(access.addr as u16);
        if access.is_read {
            self.inner
                .handle_read(port, access.width)
                .map(|v| BusResponse::Read { value: v as u64 })
                .map_err(|_| DeviceError::Internal)
        } else {
            self.inner
                .handle_write(port, access.width, access.data as usize)
                .map(|_| BusResponse::Write)
                .map_err(|_| DeviceError::Internal)
        }
    }

    fn as_any(&self) -> &dyn Any {
        &*self.inner
    }
}

#[cfg(test)]
mod tests {
    use ax_errno::AxResult;
    use axvm_types::GuestPhysAddr;

    use super::MmioDeviceAdapter;
    use crate::{
        BaseDeviceOps, Device, EmuDeviceType, GuestPhysAddrRange, Resource,
        device::{AccessWidth, BusAccess, BusKind, BusResponse},
    };

    struct MockMmioDevice {
        addr: GuestPhysAddr,
        size: usize,
        read_val: usize,
    }

    impl BaseDeviceOps<GuestPhysAddrRange> for MockMmioDevice {
        fn emu_type(&self) -> EmuDeviceType {
            EmuDeviceType::Dummy
        }
        fn address_range(&self) -> GuestPhysAddrRange {
            (self.addr..GuestPhysAddr::from(self.addr.as_usize() + self.size))
                .try_into()
                .unwrap()
        }
        fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
            Ok(self.read_val)
        }
        fn handle_write(&self, _addr: GuestPhysAddr, _width: AccessWidth, _val: usize) -> AxResult {
            Ok(())
        }
    }

    #[test]
    fn test_mmio_adapter() {
        let dev = MockMmioDevice {
            addr: GuestPhysAddr::from(0x1000),
            size: 0x100,
            read_val: 42,
        };
        let adapter = MmioDeviceAdapter::new(dev);

        let r = adapter.resources();
        assert_eq!(r.len(), 1);
        match r[0] {
            Resource::MmioRange { base, size } => {
                assert_eq!(base, 0x1000);
                assert_eq!(size, 0x100);
            }
            _ => panic!(),
        }

        let resp = adapter
            .handle(&BusAccess {
                kind: BusKind::Mmio,
                is_read: true,
                addr: 0x1000,
                width: AccessWidth::Dword,
                data: 0,
            })
            .unwrap();
        match resp {
            BusResponse::Read { value } => assert_eq!(value, 42),
            _ => panic!(),
        }

        assert!(adapter.as_any().downcast_ref::<MockMmioDevice>().is_some());
    }
}
