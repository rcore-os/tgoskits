//! x86-only port, nested-fault, and deferred exit handling.

use axvm_types::{AccessWidth, GuestPhysAddr, MappingFlags, Port};

use super::{AxvmX86Vcpu, X86_64Arch};
use crate::{
    AxVmError, AxVmResult,
    architecture::{BoundVcpuExit, CommonDeferredRunWork, VcpuRunAction},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum DeferredRunWork {
    Common(CommonDeferredRunWork),
    SysReg(super::sysreg::DeferredRunWork),
    PortRead(IoReadExit),
    PortWrite(IoWriteExit),
    NestedPageFault(NestedPageFaultExit),
    ExternalInterrupt { vector: usize },
    PreemptionTimer,
    InterruptEnd { vector: Option<u8> },
}

impl From<CommonDeferredRunWork> for DeferredRunWork {
    fn from(work: CommonDeferredRunWork) -> Self {
        Self::Common(work)
    }
}

impl From<super::sysreg::DeferredRunWork> for DeferredRunWork {
    fn from(work: super::sysreg::DeferredRunWork) -> Self {
        Self::SysReg(work)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IoReadExit {
    pub(crate) port: Port,
    pub(crate) width: AccessWidth,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct IoWriteExit {
    pub(crate) port: Port,
    pub(crate) width: AccessWidth,
    pub(crate) data: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NestedPageFaultExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) access_flags: MappingFlags,
}

pub(crate) fn handle_io_read(
    _vm: &crate::AxVM,
    _vcpu: &crate::vm::AxVCpuRef<AxvmX86Vcpu>,
    exit: IoReadExit,
) -> AxVmResult<BoundVcpuExit<DeferredRunWork>> {
    Ok(BoundVcpuExit::Defer(DeferredRunWork::PortRead(exit)))
}

pub(crate) fn handle_io_write(
    _vm: &crate::AxVM,
    exit: IoWriteExit,
) -> AxVmResult<BoundVcpuExit<DeferredRunWork>> {
    Ok(BoundVcpuExit::Defer(DeferredRunWork::PortWrite(exit)))
}

pub(crate) fn finish(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmX86Vcpu>,
    work: DeferredRunWork,
) -> AxVmResult<VcpuRunAction> {
    match work {
        DeferredRunWork::Common(work) => {
            return crate::architecture::finish_deferred::<X86_64Arch>(vm, vcpu, work);
        }
        DeferredRunWork::SysReg(work) => {
            return super::sysreg::finish(vm, vcpu, work);
        }
        DeferredRunWork::PortRead(exit) => {
            let value = vm
                .get_devices()?
                .handle_port_read(exit.port, exit.width)
                .map_err(|error| AxVmError::device("read guest I/O port", error))?;
            vcpu.set_gpr(0, value);
        }
        DeferredRunWork::PortWrite(exit) => {
            vm.get_devices()?
                .handle_port_write(exit.port, exit.width, exit.data as usize)
                .map_err(|error| AxVmError::device("write guest I/O port", error))?;
        }
        DeferredRunWork::NestedPageFault(exit) => {
            return super::handle_x86_nested_page_fault(vm, exit);
        }
        DeferredRunWork::ExternalInterrupt { vector } => {
            X86_64Arch::after_external_interrupt(vm, vcpu, vector);
        }
        DeferredRunWork::PreemptionTimer => {
            super::irq::inject_due_pit_irq0(vm, vcpu);
            super::irq::inject_pending_serial_irq(vm, vcpu);
        }
        DeferredRunWork::InterruptEnd { vector } => {
            if let Some(vector) = vector {
                super::irq::inject_pending_ioapic_irq_after_eoi(vm, vcpu, vector);
            }
        }
    }
    Ok(VcpuRunAction {
        waits_for_event: false,
        stop_reason: None,
    })
}
