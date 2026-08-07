//! x86-only port, nested-fault, and deferred exit handling.

use axvm_types::{AccessWidth, GuestPhysAddr, MappingFlags, Port};
use x86_vcpu::{X86PortIoDirection, X86PortIoStringExit};

use super::*;

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
    let devices = vm.get_devices()?;
    let val = devices
        .try_handle_port_read(exit.port, exit.width)
        .map_err(|error| AxVmError::device("read guest I/O port", error))?
        .unwrap_or_else(|| unmapped_port_value(exit.width));
    vcpu.set_gpr(0, val);
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn handle_io_write(
    vm: &crate::AxVM,
    exit: IoWriteExit,
) -> AxVmResult<BoundVcpuExit<DeferredRunWork>> {
    vm.try_handle_port_write(exit.port, exit.width, exit.data as usize)
        .map_err(|error| AxVmError::device("write guest I/O port", error))?;
    Ok(BoundVcpuExit::Continue)
}

pub(crate) fn handle_io_string(
    vm: &crate::AxVM,
    vcpu: &crate::vm::AxVCpuRef<AxvmX86Vcpu>,
    exit: X86PortIoStringExit,
) -> AxVmResult<BoundVcpuExit<DeferredRunWork>> {
    let port = super::x86_port_to_ax(exit.port());
    let width = super::x86_access_width_to_ax(exit.width());
    let size = width.size();
    let guest_paddr = super::x86_guest_phys_addr_to_ax(exit.guest_paddr());

    match exit.direction() {
        X86PortIoDirection::In => {
            let devices = vm.get_devices()?;
            let value = devices
                .try_handle_port_read(port, width)
                .map_err(|error| AxVmError::device("read guest string I/O port", error))?
                .unwrap_or_else(|| unmapped_port_value(width));
            vm.write_to_guest(guest_paddr, &value.to_le_bytes()[..size])?;
        }
        X86PortIoDirection::Out => {
            let mut bytes = [0u8; 8];
            vm.read_from_guest(guest_paddr, &mut bytes[..size])?;
            let value = u64::from_le_bytes(bytes);
            vm.try_handle_port_write(port, width, value as usize)
                .map_err(|error| AxVmError::device("write guest string I/O port", error))?;
        }
    }

    vcpu.get_arch_vcpu().complete_port_io_string(exit)?;
    Ok(BoundVcpuExit::Continue)
}

fn unmapped_port_value(width: AccessWidth) -> usize {
    usize::MAX >> ((core::mem::size_of::<usize>() - width.size()) * 8)
}

pub(crate) fn finish(
    vm: &crate::AxVMRef,
    vcpu: &crate::vm::AxVCpuRef<AxvmX86Vcpu>,
    work: DeferredRunWork,
) -> AxVmResult<VcpuRunAction> {
    match work {
        DeferredRunWork::ExternalInterrupt { vector } => {
            X86_64Arch::after_external_interrupt(vm, vcpu, vector);
        }
        DeferredRunWork::PreemptionTimer => {
            crate::timer::check_events();
            super::irq::inject_due_pit_irq0(vm, vcpu);
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
        resets_vm: false,
        exits_vcpu: false,
    })
}
