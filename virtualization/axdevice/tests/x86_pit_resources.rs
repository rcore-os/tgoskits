#![cfg(target_arch = "x86_64")]

use axdevice::{Device, X86PitDevice};
use axdevice_base::Resource;
use x86_vlapic::{
    X86HostPhysAddr, X86HostVirtAddr, X86InterruptVector, X86TimerCallback, X86VcpuId,
    X86VlapicHostOps, X86VlapicResult, X86VmId,
};

#[test]
fn pit_declares_only_its_discrete_port_ranges() {
    let pit = X86PitDevice::<TestHost>::new();

    assert_eq!(
        pit.resources(),
        &[
            Resource::PortRange {
                base: 0x40,
                size: 4,
            },
            Resource::PortRange {
                base: 0x61,
                size: 1,
            },
        ]
    );
}

struct TestHost;

impl X86VlapicHostOps for TestHost {
    fn alloc_frame() -> Option<X86HostPhysAddr> {
        None
    }

    fn dealloc_frame(_paddr: X86HostPhysAddr) {}

    fn phys_to_virt(paddr: X86HostPhysAddr) -> X86HostVirtAddr {
        X86HostVirtAddr::from_usize(paddr.as_usize())
    }

    fn virt_to_phys(vaddr: X86HostVirtAddr) -> X86HostPhysAddr {
        X86HostPhysAddr::from_usize(vaddr.as_usize())
    }

    fn current_time_nanos() -> u64 {
        0
    }

    fn register_timer(_deadline_nanos: u64, _callback: X86TimerCallback) -> Option<usize> {
        None
    }

    fn cancel_timer(_token: usize) {}

    fn current_vm_id() -> X86VmId {
        0
    }

    fn current_vm_vcpu_num() -> usize {
        1
    }

    fn current_vm_active_vcpus() -> usize {
        1
    }

    fn active_vcpus(_vm_id: X86VmId) -> Option<usize> {
        Some(1)
    }

    fn inject_interrupt(
        _vm_id: X86VmId,
        _vcpu_id: X86VcpuId,
        _vector: X86InterruptVector,
    ) -> X86VlapicResult {
        Ok(())
    }
}
