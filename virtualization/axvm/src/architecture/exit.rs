//! Architecture-neutral handlers for exits shared by every guest architecture.
//!
//! The `handle_*` entrypoints run while the backend still owns host CPU state,
//! so they only copy fixed-size exit data into a deferred-work value. The
//! corresponding device, hypercall, and guest-register operations are owned by
//! `finish_deferred`, after backend unbind and CPU-pin release.

use axvm_types::VmArchVcpuOps;

use super::{
    ArchOps, BoundVcpuExit, CommonDeferredRunWork, HypercallExit, MmioReadExit, MmioWriteExit,
    VcpuRunAction,
};
use crate::{AxVmError, AxVmResult};

pub(crate) fn handle_mmio_read<A: ArchOps>(
    _vm: &crate::AxVMRef,
    _vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: MmioReadExit,
) -> AxVmResult<BoundVcpuExit<A::DeferredRunWork>> {
    Ok(BoundVcpuExit::Defer(
        CommonDeferredRunWork::MmioRead(exit).into(),
    ))
}

pub(crate) fn handle_mmio_write<A: ArchOps>(
    _vm: &crate::AxVMRef,
    _vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: MmioWriteExit,
) -> AxVmResult<BoundVcpuExit<A::DeferredRunWork>> {
    Ok(BoundVcpuExit::Defer(
        CommonDeferredRunWork::MmioWrite(exit).into(),
    ))
}

pub(crate) fn handle_hypercall<V, D>(
    _vm: &crate::AxVMRef,
    _vcpu: &crate::vm::AxVCpuRef<V>,
    exit: HypercallExit,
) -> AxVmResult<BoundVcpuExit<D>>
where
    V: VmArchVcpuOps,
    D: From<CommonDeferredRunWork>,
{
    Ok(BoundVcpuExit::Defer(
        CommonDeferredRunWork::Hypercall(exit).into(),
    ))
}

/// Executes architecture-neutral exit work with normal host preemption.
pub(crate) fn finish_deferred<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    work: CommonDeferredRunWork,
) -> AxVmResult<VcpuRunAction> {
    match work {
        CommonDeferredRunWork::Hypercall(exit) => finish_hypercall(vm, vcpu, exit),
        CommonDeferredRunWork::MmioRead(exit) => finish_mmio_read::<A>(vm, vcpu, exit)?,
        CommonDeferredRunWork::MmioWrite(exit) => finish_mmio_write::<A>(vm, vcpu, exit)?,
    }
    Ok(resume_action())
}

fn finish_hypercall<V: VmArchVcpuOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<V>,
    exit: HypercallExit,
) {
    debug!("Hypercall [{:#x}] args {:x?}", exit.nr, exit.args);
    match crate::runtime::hvc::HyperCall::new(vm.clone(), exit.nr, exit.args) {
        Ok(hypercall) => {
            let ret_val = match hypercall.execute() {
                Ok(ret_val) => ret_val as isize,
                Err(error) => {
                    let err = AxVmError::from(error);
                    warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
                    -1
                }
            };
            vcpu.set_return_value(ret_val as usize);
        }
        Err(error) => {
            let err = AxVmError::from(error);
            warn!("Hypercall [{:#x}] failed: {err:?}", exit.nr);
        }
    }
}

fn finish_mmio_read<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: MmioReadExit,
) -> AxVmResult {
    let raw = vm
        .get_devices()?
        .handle_mmio_read(exit.addr, exit.width)
        .map_err(|error| AxVmError::device("read guest MMIO", error))?;
    let masked = raw & crate::vm::width_mask(exit.width);
    let value = if exit.signed_ext {
        crate::vm::sign_extend_value(masked, exit.width)
    } else {
        masked & crate::vm::width_mask(exit.reg_width)
    };
    vcpu.set_gpr(exit.reg, value);
    A::after_mmio_read(vm, vcpu)
}

fn finish_mmio_write<A: ArchOps>(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<A::VCpu>,
    exit: MmioWriteExit,
) -> AxVmResult {
    vm.handle_mmio_write(exit.addr, exit.width, exit.data as usize)?;
    A::after_mmio_write(vm, vcpu)
}

const fn resume_action() -> VcpuRunAction {
    VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
    }
}
