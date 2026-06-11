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

use std::sync::{Arc, Mutex};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PhysAddr, VirtAddr};
use axdevice::{AxVmDeviceConfig, AxVmDevices};
use axdevice_base::{AccessWidth, BaseDeviceOps, Port, PortRange, SysRegAddr, SysRegAddrRange};
use axvm_types::{
    EmulatedDeviceConfig, EmulatedDeviceType, GuestPhysAddr, GuestPhysAddrRange, InterruptVector,
    VCpuId, VMId,
};
use x86_vlapic::host::X86VlapicHostIf;

struct MockMmioDevice {
    name: String,
    range: GuestPhysAddrRange,
    last_write: Mutex<Option<(usize, usize)>>,
}

impl MockMmioDevice {
    fn new(name: &str, base: usize, len: usize) -> Self {
        let start = GuestPhysAddr::from(base);
        let end = GuestPhysAddr::from(base + len);

        Self::with_range(name, GuestPhysAddrRange::new(start, end))
    }

    fn with_range(name: &str, range: GuestPhysAddrRange) -> Self {
        Self {
            name: String::from(name),
            range,
            last_write: Mutex::new(None),
        }
    }

    fn get_last_write(&self) -> Option<(usize, usize)> {
        *self.last_write.lock().unwrap()
    }
}

impl BaseDeviceOps<GuestPhysAddrRange> for MockMmioDevice {
    fn address_range(&self) -> GuestPhysAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::IVCChannel
    }

    fn handle_read(&self, _addr: GuestPhysAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0xDEAD_BEEF)
    }

    fn handle_write(&self, addr: GuestPhysAddr, _width: AccessWidth, val: usize) -> AxResult {
        println!(
            "[Test] Device {} write: addr={:?}, val={:#x}",
            self.name, addr, val
        );

        let offset = addr.as_usize() - self.range.start.as_usize();
        *self.last_write.lock().unwrap() = Some((offset, val));
        Ok(())
    }
}

struct MockPortDevice {
    range: PortRange,
}

impl MockPortDevice {
    fn new(start: u16, end: u16) -> Self {
        Self {
            range: PortRange::new(Port::new(start), Port::new(end)),
        }
    }
}

impl BaseDeviceOps<PortRange> for MockPortDevice {
    fn address_range(&self) -> PortRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Console
    }

    fn handle_read(&self, _addr: Port, _width: AccessWidth) -> AxResult<usize> {
        Ok(0)
    }

    fn handle_write(&self, _addr: Port, _width: AccessWidth, _val: usize) -> AxResult {
        Ok(())
    }
}

struct MockSysRegDevice {
    range: SysRegAddrRange,
}

impl MockSysRegDevice {
    fn new(start: usize, end: usize) -> Self {
        Self {
            range: SysRegAddrRange::new(SysRegAddr::new(start), SysRegAddr::new(end)),
        }
    }
}

impl BaseDeviceOps<SysRegAddrRange> for MockSysRegDevice {
    fn address_range(&self) -> SysRegAddrRange {
        self.range
    }

    fn emu_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::InterruptController
    }

    fn handle_read(&self, _addr: SysRegAddr, _width: AccessWidth) -> AxResult<usize> {
        Ok(0)
    }

    fn handle_write(&self, _addr: SysRegAddr, _width: AccessWidth, _val: usize) -> AxResult {
        Ok(())
    }
}

fn empty_devices() -> AxVmDevices {
    AxVmDevices::new(AxVmDeviceConfig::new(vec![])).unwrap()
}

fn mmio_device(name: &str, start: usize, end: usize) -> Arc<MockMmioDevice> {
    Arc::new(MockMmioDevice::with_range(
        name,
        GuestPhysAddrRange::new(start.into(), end.into()),
    ))
}

#[test]
fn test_mmio_dispatch_functionality() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config).unwrap();

    let base_addr = 0x1000_0000;
    let dev_size = 0x1000;
    let mock_dev = Arc::new(MockMmioDevice::new("TestDev", base_addr, dev_size));

    devices.add_mmio_dev(mock_dev.clone()).unwrap();

    let write_offset = 0x40;
    let target_addr = GuestPhysAddr::from(base_addr + write_offset);
    let write_val = 0x1234_5678;

    let width = AccessWidth::try_from(4).unwrap();

    devices
        .handle_mmio_write(target_addr, width, write_val)
        .expect("MMIO write failed");

    let last = mock_dev.get_last_write();
    assert!(last.is_some(), "Device did not receive write command");
    let (off, val) = last.unwrap();
    assert_eq!(off, write_offset, "Write offset mismatch");
    assert_eq!(val, write_val, "Write value mismatch");

    let read_result = devices
        .handle_mmio_read(target_addr, width)
        .expect("MMIO read failed");

    assert_eq!(read_result, 0xDEAD_BEEF, "Read value mismatch");
}

#[test]
#[should_panic(expected = "emu_device not found")]
fn test_mmio_panic_on_missing_device() {
    let config = AxVmDeviceConfig::new(vec![]);
    let devices = AxVmDevices::new(config).unwrap();

    let invalid_addr = GuestPhysAddr::from(0x9999_9999);
    let width = AccessWidth::try_from(4).unwrap();

    let _ = devices.handle_mmio_read(invalid_addr, width);
}

#[test]
fn test_mmio_adjacent_ranges_are_allowed() {
    let mut devices = empty_devices();

    assert_eq!(
        devices.add_mmio_dev(mmio_device("first", 0x1000, 0x2000)),
        Ok(())
    );
    assert_eq!(
        devices.add_mmio_dev(mmio_device("adjacent", 0x2000, 0x3000)),
        Ok(())
    );
    assert_eq!(devices.iter_mmio_dev().count(), 2);
}

#[test]
fn test_mmio_duplicate_and_overlapping_ranges_are_rejected_without_modification() {
    let mut devices = empty_devices();
    let existing = mmio_device("existing", 0x2000, 0x3000);

    assert_eq!(devices.add_mmio_dev(existing.clone()), Ok(()));
    assert_eq!(devices.add_mmio_dev(existing), Err(AxError::AlreadyExists));
    assert_eq!(
        devices.add_mmio_dev(mmio_device("same-range", 0x2000, 0x3000)),
        Err(AxError::AlreadyExists)
    );
    assert_eq!(
        devices.add_mmio_dev(mmio_device("partial-left", 0x1800, 0x2800)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        devices.add_mmio_dev(mmio_device("partial-right", 0x2800, 0x3800)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        devices.add_mmio_dev(mmio_device("contains", 0x1000, 0x4000)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        devices.add_mmio_dev(mmio_device("contained", 0x2400, 0x2800)),
        Err(AxError::AddrInUse)
    );
    assert_eq!(devices.iter_mmio_dev().count(), 1);
}

#[test]
fn test_empty_and_wrapped_ranges_are_rejected() {
    let mut devices = empty_devices();
    let empty_mmio = Arc::new(MockMmioDevice::with_range(
        "empty-mmio",
        GuestPhysAddrRange::new(0x1000.into(), 0x1000.into()),
    ));
    let wrapped_mmio = Arc::new(MockMmioDevice::with_range(
        "wrapped-mmio",
        GuestPhysAddrRange {
            start: (usize::MAX - 0xf).into(),
            end: 0x10.into(),
        },
    ));
    let invalid_port = Arc::new(MockPortDevice::new(0x400, 0x3ff));
    let invalid_sysreg = Arc::new(MockSysRegDevice::new(0x101, 0x100));

    assert_eq!(devices.add_mmio_dev(empty_mmio), Err(AxError::InvalidInput));
    assert_eq!(
        devices.add_mmio_dev(wrapped_mmio),
        Err(AxError::InvalidInput)
    );
    assert_eq!(
        devices.add_port_dev(invalid_port),
        Err(AxError::InvalidInput)
    );
    assert_eq!(
        devices.add_sys_reg_dev(invalid_sysreg),
        Err(AxError::InvalidInput)
    );
    assert_eq!(devices.iter_mmio_dev().count(), 0);
    assert_eq!(devices.iter_port_dev().count(), 0);
    assert_eq!(devices.iter_sys_reg_dev().count(), 0);
}

#[test]
fn test_port_inclusive_endpoint_overlap_is_rejected() {
    let mut devices = empty_devices();

    assert_eq!(
        devices.add_port_dev(Arc::new(MockPortDevice::new(0x3f8, 0x3ff))),
        Ok(())
    );
    assert_eq!(
        devices.add_port_dev(Arc::new(MockPortDevice::new(0x3ff, 0x400))),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        devices.add_port_dev(Arc::new(MockPortDevice::new(0x400, 0x400))),
        Ok(())
    );
    assert_eq!(devices.iter_port_dev().count(), 2);
}

#[test]
fn test_sysreg_inclusive_endpoint_overlap_is_rejected() {
    let mut devices = empty_devices();

    assert_eq!(
        devices.add_sys_reg_dev(Arc::new(MockSysRegDevice::new(0x100, 0x110))),
        Ok(())
    );
    assert_eq!(
        devices.add_sys_reg_dev(Arc::new(MockSysRegDevice::new(0x110, 0x120))),
        Err(AxError::AddrInUse)
    );
    assert_eq!(
        devices.add_sys_reg_dev(Arc::new(MockSysRegDevice::new(0x111, 0x120))),
        Ok(())
    );
    assert_eq!(devices.iter_sys_reg_dev().count(), 2);
}

#[test]
fn test_equal_address_values_on_different_buses_are_allowed() {
    let mut devices = empty_devices();

    assert_eq!(
        devices.add_mmio_dev(mmio_device("mmio", 0x1000, 0x1001)),
        Ok(())
    );
    assert_eq!(
        devices.add_port_dev(Arc::new(MockPortDevice::new(0x1000, 0x1000))),
        Ok(())
    );
    assert_eq!(
        devices.add_sys_reg_dev(Arc::new(MockSysRegDevice::new(0x1000, 0x1000))),
        Ok(())
    );
    assert_eq!(devices.iter_mmio_dev().count(), 1);
    assert_eq!(devices.iter_port_dev().count(), 1);
    assert_eq!(devices.iter_sys_reg_dev().count(), 1);
}

#[test]
fn test_conflicting_device_config_returns_structured_error() {
    let ioapic = EmulatedDeviceConfig {
        name: String::from("ioapic"),
        base_gpa: 0xfec0_0000,
        length: 0x1000,
        irq_id: 0,
        emu_type: EmulatedDeviceType::X86IoApic,
        cfg_list: vec![],
    };

    assert_eq!(
        AxVmDevices::new(AxVmDeviceConfig::new(vec![ioapic.clone(), ioapic])).err(),
        Some(AxError::AlreadyExists)
    );
}

// Mock implementation for x86_vlapic host callbacks when running
// `cargo test -p axdevice` on x86_64 host.

struct MockX86VlapicHostIfImpl;

#[ax_crate_interface::impl_interface]
impl X86VlapicHostIf for MockX86VlapicHostIfImpl {
    fn alloc_frame() -> Option<PhysAddr> {
        None
    }

    fn dealloc_frame(_paddr: PhysAddr) {}

    fn phys_to_virt(paddr: PhysAddr) -> VirtAddr {
        VirtAddr::from(paddr.as_usize())
    }

    fn virt_to_phys(vaddr: VirtAddr) -> PhysAddr {
        PhysAddr::from(vaddr.as_usize())
    }

    fn current_time() -> core::time::Duration {
        core::time::Duration::ZERO
    }

    fn current_time_nanos() -> u64 {
        0
    }

    fn register_timer(
        _deadline: core::time::Duration,
        _callback: Box<dyn FnOnce(core::time::Duration) + Send + 'static>,
    ) -> usize {
        0
    }

    fn cancel_timer(_token: usize) {}

    fn write_bytes(_bytes: &[u8]) {}

    fn read_bytes(_bytes: &mut [u8]) -> usize {
        0
    }

    fn current_vm_id() -> VMId {
        0
    }

    fn current_vm_vcpu_num() -> usize {
        1
    }

    fn current_vm_active_vcpus() -> usize {
        1
    }

    fn active_vcpus(_vm_id: VMId) -> Option<usize> {
        Some(1)
    }

    fn inject_interrupt(_vm_id: VMId, _vcpu_id: VCpuId, _vector: InterruptVector) -> AxResult {
        Ok(())
    }
}
