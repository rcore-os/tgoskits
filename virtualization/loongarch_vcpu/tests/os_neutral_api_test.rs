#![cfg(target_arch = "loongarch64")]

use alloc::boxed::Box;
use core::time::Duration;

extern crate alloc;

use loongarch_vcpu::{
    LoongArchAccessFlags, LoongArchAccessWidth, LoongArchGuestPhysAddr, LoongArchHostOps,
    LoongArchHostPhysAddr, LoongArchHostVirtAddr, LoongArchNestedPagingConfig, LoongArchVCpu,
    LoongArchVcpu, LoongArchVcpuError, LoongArchVcpuResult, LoongArchVmExit,
};

struct DummyHost;

impl LoongArchHostOps for DummyHost {
    fn virt_to_phys(vaddr: LoongArchHostVirtAddr) -> LoongArchHostPhysAddr {
        LoongArchHostPhysAddr::from_usize(vaddr.as_usize())
    }

    fn current_time_nanos() -> u64 {
        0
    }

    fn ticks_to_nanos(ticks: u64) -> u64 {
        ticks
    }

    fn register_timer(
        _deadline: Duration,
        _callback: Box<dyn FnOnce(Duration) + Send + 'static>,
    ) -> usize {
        0
    }

    fn cancel_timer(_token: usize) {}

    fn inject_interrupt(_vm_id: usize, _vcpu_id: usize, _vector: usize) {}
}

#[test]
fn vcpu_type_is_host_generic_without_axvm_traits() {
    let _vcpu: Option<LoongArchVcpu<DummyHost>> = None;
    let _compat_vcpu: Option<LoongArchVCpu<DummyHost>> = None;
}

#[test]
fn nested_paging_config_uses_os_neutral_integer_values() {
    let config = LoongArchNestedPagingConfig::new(0x1000, 4, 48, 0);

    assert_eq!(config.root_paddr.as_usize(), 0x1000);
    assert_eq!(config.levels, 4);
    assert_eq!(config.gpa_bits, 48);
    assert_eq!(config.mode, 0);
}

#[test]
fn vm_exit_types_are_defined_by_loongarch_vcpu_core() {
    let exit = LoongArchVmExit::MmioRead {
        addr: LoongArchGuestPhysAddr::from_usize(0x2000),
        width: LoongArchAccessWidth::Dword,
        reg: 3,
        reg_width: LoongArchAccessWidth::Qword,
        signed_ext: false,
    };

    match exit {
        LoongArchVmExit::MmioRead {
            addr, width, reg, ..
        } => {
            assert_eq!(addr.as_usize(), 0x2000);
            assert_eq!(width.size(), 4);
            assert_eq!(reg, 3);
        }
        other => panic!("unexpected exit: {other:?}"),
    }

    let exit = LoongArchVmExit::NestedPageFault {
        addr: LoongArchGuestPhysAddr::from_usize(0x1000),
        access_flags: LoongArchAccessFlags::READ | LoongArchAccessFlags::WRITE,
    };
    assert!(matches!(
        exit,
        LoongArchVmExit::NestedPageFault {
            addr,
            access_flags,
        } if addr.as_usize() == 0x1000
            && access_flags.contains(LoongArchAccessFlags::READ)
            && access_flags.contains(LoongArchAccessFlags::WRITE)
    ));
}

#[test]
fn host_ops_can_report_typed_errors() {
    assert_eq!(
        Err(LoongArchVcpuError::Unsupported),
        unsupported_host_call()
    );
}

fn unsupported_host_call() -> LoongArchVcpuResult {
    Err(LoongArchVcpuError::Unsupported)
}
