//! x86-only port, nested-fault, and deferred exit handling.

use axvm_types::{AccessWidth, GuestPhysAddr, MappingFlags, Port};

use super::{ArchOps, AxvmX86Vcpu, X86_64Arch};
use crate::{
    AxVmError, AxVmResult,
    architecture::{BoundVcpuExit, VcpuRunAction},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum DeferredRunWork {
    ExternalInterrupt { vector: usize },
    PreemptionTimer,
    InterruptEnd { vector: Option<u8> },
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
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<AxvmX86Vcpu>,
    exit: IoReadExit,
) -> AxVmResult<BoundVcpuExit<DeferredRunWork>> {
    let val = vm
        .get_devices()?
        .handle_port_read(exit.port, exit.width)
        .map_err(|error| AxVmError::device("read guest I/O port", error))?;
    vcpu.set_gpr(0, val);
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn handle_io_write(
    vm: &crate::AxVM,
    exit: IoWriteExit,
) -> AxVmResult<BoundVcpuExit<DeferredRunWork>> {
    vm.get_devices()?
        .handle_port_write(exit.port, exit.width, exit.data as usize)
        .map_err(|error| AxVmError::device("write guest I/O port", error))?;
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn finish(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmX86Vcpu>,
    work: DeferredRunWork,
) -> AxVmResult<VcpuRunAction> {
    let action = match work {
        DeferredRunWork::ExternalInterrupt { vector } => {
            X86_64Arch::after_external_interrupt(vm, vcpu, vector);
            VcpuRunAction::resume()
        }
        DeferredRunWork::PreemptionTimer => {
            crate::timer::check_events();
            super::irq::inject_due_pit_irq0(vm, vcpu);
            super::irq::inject_pending_serial_irq(vm, vcpu);
            VcpuRunAction::new(crate::architecture::VcpuScheduling::YIELD, None)
        }
        DeferredRunWork::InterruptEnd { vector } => {
            if let Some(vector) = vector {
                super::irq::inject_pending_ioapic_irq_after_eoi(vm, vcpu, vector);
            }
            VcpuRunAction::resume()
        }
    };
    Ok(action)
}
