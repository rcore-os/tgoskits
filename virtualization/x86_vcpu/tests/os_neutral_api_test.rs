// Copyright 2026 The Axvisor Team
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

#![cfg(target_arch = "x86_64")]

use x86_vcpu::{
    X86AccessFlags, X86AccessWidth, X86GuestPhysAddr, X86HostOps, X86HostPhysAddr, X86HostVirtAddr,
    X86MsrAddr, X86NestedPagingConfig, X86Port, X86VcpuCreateConfig, X86VcpuError, X86VcpuResult,
    X86VcpuSetupConfig, X86VmExit,
};
use x86_vlapic::X86VlapicHostOps;

struct DummyHost;

impl X86VlapicHostOps for DummyHost {
    fn alloc_frame() -> Option<x86_vlapic::X86HostPhysAddr> {
        None
    }

    fn dealloc_frame(_paddr: x86_vlapic::X86HostPhysAddr) {}

    fn phys_to_virt(paddr: x86_vlapic::X86HostPhysAddr) -> x86_vlapic::X86HostVirtAddr {
        x86_vlapic::X86HostVirtAddr::from_usize(paddr.as_usize())
    }

    fn virt_to_phys(vaddr: x86_vlapic::X86HostVirtAddr) -> x86_vlapic::X86HostPhysAddr {
        x86_vlapic::X86HostPhysAddr::from_usize(vaddr.as_usize())
    }

    fn current_time_nanos() -> u64 {
        0
    }

    fn register_timer(
        _deadline_nanos: u64,
        _callback: x86_vlapic::X86TimerCallback,
    ) -> Option<usize> {
        None
    }

    fn cancel_timer(_token: usize) {}

    fn write_bytes(_bytes: &[u8]) {}

    fn read_bytes(_bytes: &mut [u8]) -> usize {
        0
    }

    fn current_vm_id() -> x86_vlapic::X86VmId {
        0
    }

    fn current_vm_vcpu_num() -> usize {
        1
    }

    fn current_vm_active_vcpus() -> usize {
        1
    }

    fn active_vcpus(_vm_id: x86_vlapic::X86VmId) -> Option<usize> {
        Some(1)
    }

    fn inject_interrupt(
        _vm_id: x86_vlapic::X86VmId,
        _vcpu_id: x86_vlapic::X86VcpuId,
        _vector: x86_vlapic::X86InterruptVector,
    ) -> x86_vlapic::X86VlapicResult {
        Ok(())
    }
}

impl X86HostOps for DummyHost {
    fn alloc_frame() -> Option<X86HostPhysAddr> {
        None
    }

    fn dealloc_frame(_paddr: X86HostPhysAddr) {}

    fn alloc_contiguous_frames(
        _frame_count: usize,
        _frame_align: usize,
    ) -> Option<X86HostPhysAddr> {
        None
    }

    fn dealloc_contiguous_frames(_start_paddr: X86HostPhysAddr, _frame_count: usize) {}

    fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr {
        X86HostVirtAddr::from_usize(paddr.as_usize())
    }

    fn read_guest_u8(_paddr: X86GuestPhysAddr) -> X86VcpuResult<u8> {
        Err(X86VcpuError::Unsupported)
    }

    fn nanos_to_ticks(nanos: u64) -> u64 {
        nanos
    }

    fn poll_host_interrupt() -> Option<u8> {
        None
    }
}

#[test]
fn x86_value_types_are_os_neutral() {
    let gpa = X86GuestPhysAddr::from_usize(0xfee0_0000);
    let hpa = X86HostPhysAddr::from_usize(0x1000);
    let hva = X86HostVirtAddr::from_usize(0xffff_8000_0000_1000);
    let port = X86Port::new(0x3f8);
    let msr = X86MsrAddr::new(0x800);

    assert_eq!(gpa.as_usize(), 0xfee0_0000);
    assert_eq!(hpa.as_usize(), 0x1000);
    assert_eq!(hva.as_usize(), 0xffff_8000_0000_1000);
    assert_eq!(port.number(), 0x3f8);
    assert_eq!(msr.addr(), 0x800);
    assert_eq!(X86AccessWidth::Dword.size(), 4);
}

#[test]
fn nested_paging_config_uses_x86_local_types() {
    let config = X86NestedPagingConfig::new(X86HostPhysAddr::from_usize(0x2000), 4, 48, 0);

    assert_eq!(config.root_paddr.as_usize(), 0x2000);
    assert_eq!(config.levels, 4);
    assert_eq!(config.gpa_bits, 48);
    assert_eq!(config.mode, 0);
}

#[test]
fn setup_config_reports_x86_errors() {
    let mut config = X86VcpuSetupConfig::default();

    assert_eq!(
        config.add_passthrough_port_range(0x6000, 0),
        Err(X86VcpuError::InvalidInput)
    );
}

#[test]
fn vm_exit_types_are_defined_by_x86_vcpu_core() {
    let exit = X86VmExit::PortIoWrite {
        port: X86Port::new(0x604),
        width: X86AccessWidth::Word,
        data: 0x2000,
    };
    assert!(matches!(
        exit,
        X86VmExit::PortIoWrite {
            port,
            width: X86AccessWidth::Word,
            data: 0x2000,
        } if port.number() == 0x604
    ));

    let exit = X86VmExit::NestedPageFault {
        addr: X86GuestPhysAddr::from_usize(0xfec0_0000),
        access_flags: X86AccessFlags::WRITE,
    };
    assert!(matches!(
        exit,
        X86VmExit::NestedPageFault { addr, access_flags }
            if addr.as_usize() == 0xfec0_0000 && access_flags.contains(X86AccessFlags::WRITE)
    ));
}

#[test]
fn host_ops_can_report_typed_errors() {
    assert_eq!(
        Err(X86VcpuError::Unsupported),
        DummyHost::read_guest_u8(0.into())
    );
}

#[test]
fn create_config_is_not_tied_to_axvm_traits() {
    let _create = X86VcpuCreateConfig;
    let _setup = X86VcpuSetupConfig::default();
    let _host: Option<DummyHost> = None;
}
