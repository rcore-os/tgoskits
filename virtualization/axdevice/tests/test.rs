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

use ax_errno::AxResult;
use ax_memory_addr::{PhysAddr, VirtAddr};
use axdevice::{AxVmDeviceConfig, AxVmDevices};
use axdevice_base::{AccessWidth, BaseDeviceOps};
use axvm_types::{GuestPhysAddr, GuestPhysAddrRange, InterruptVector, VCpuId, VMId};
use axvmconfig::EmulatedDeviceType;
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

        Self {
            name: String::from(name),
            range: GuestPhysAddrRange::new(start, end),
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

#[test]
fn test_mmio_dispatch_functionality() {
    let config = AxVmDeviceConfig::new(vec![]);
    let mut devices = AxVmDevices::new(config);

    let base_addr = 0x1000_0000;
    let dev_size = 0x1000;
    let mock_dev = Arc::new(MockMmioDevice::new("TestDev", base_addr, dev_size));

    devices.add_mmio_dev(mock_dev.clone());

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
    let devices = AxVmDevices::new(config);

    let invalid_addr = GuestPhysAddr::from(0x9999_9999);
    let width = AccessWidth::try_from(4).unwrap();

    let _ = devices.handle_mmio_read(invalid_addr, width);
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
