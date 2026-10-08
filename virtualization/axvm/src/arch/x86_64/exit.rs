//! x86-only port and nested-fault exit handling.
//!
//! These helpers run in the owning vCPU task after the architecture backend has
//! been unloaded, so they may use the sleepable run services and device runtime.
//! Every register effect is returned as an owned [`X86Completion`] and committed
//! on the next bound guest entry.

use axdevice_base::{BusKind, DeviceAccess, DeviceVcpuId};
use axvm_types::{AccessWidth, GuestPhysAddr, MappingFlags, Port};

use super::*;
use crate::arch::x86_64::policy::{X86PortIoDirection, X86PortIoStringExit};

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

fn port_access(vcpu_id: usize, port: Port, width: AccessWidth) -> DeviceAccess {
    DeviceAccess::new(
        DeviceVcpuId::new(vcpu_id),
        BusKind::Port,
        port.number() as u64,
        width,
    )
}

pub(crate) fn handle_io_read(
    services: &RunServices,
    vcpu_id: usize,
    exit: IoReadExit,
) -> AxVmResult<VcpuAction<X86Completion, GuestRequest>> {
    let access = port_access(vcpu_id, exit.port, exit.width);
    let value = services
        .read_device(&access)?
        .map(|value| value as usize)
        .unwrap_or_else(|| unmapped_port_value(exit.width));
    // IN preserves the remaining accumulator bits for byte and word operands.
    let completion = match exit.width {
        AccessWidth::Byte => X86Completion::ByteGpr {
            register: policy::X86ByteRegister {
                gpr: 0,
                high: false,
            },
            value: value as u8,
        },
        AccessWidth::Word => X86Completion::WordGpr {
            register: 0,
            value: value as u16,
        },
        AccessWidth::Dword => RegisterCompletion::Gpr {
            register: 0,
            value: value as u32 as usize,
        }
        .into(),
        AccessWidth::Qword => {
            return Err(AxVmError::invalid_input(
                "read x86 I/O port",
                "IN has no 64-bit operand",
            ));
        }
    };
    Ok(VcpuAction::Reenter(completion))
}

pub(crate) fn handle_io_write(
    services: &RunServices,
    vcpu_id: usize,
    exit: IoWriteExit,
) -> AxVmResult<VcpuAction<X86Completion, GuestRequest>> {
    let access = port_access(vcpu_id, exit.port, exit.width);
    services.write_device(&access, exit.data)?;
    publish_pic_interrupt_if_needed(services, vcpu_id, exit.port)?;
    Ok(VcpuAction::Reenter(X86Completion::default()))
}

pub(crate) fn handle_io_string(
    services: &RunServices,
    vcpu_id: usize,
    exit: X86PortIoStringExit,
) -> AxVmResult<VcpuAction<X86Completion, GuestRequest>> {
    let port = super::x86_port_to_ax(exit.port());
    let width = super::x86_access_width_to_ax(exit.width());
    let size = width.size();
    let guest_paddr = super::x86_guest_phys_addr_to_ax(exit.guest_paddr());
    let access = port_access(vcpu_id, port, width);

    match exit.direction() {
        X86PortIoDirection::In => {
            let value = services
                .read_device(&access)?
                .map(|value| value as usize)
                .unwrap_or_else(|| unmapped_port_value(width));
            let bytes = value.to_le_bytes();
            services
                .memory()
                .with_access(|memory| memory.write(guest_paddr, &bytes[..size]))
                .and_then(|result| result)
                .map_err(|error| AxVmError::device("write guest string I/O memory", error))?;
        }
        X86PortIoDirection::Out => {
            let bytes = services
                .memory()
                .with_access(|memory| {
                    let mut bytes = [0u8; 8];
                    memory.read(guest_paddr, &mut bytes[..size])?;
                    Ok::<_, axdevice_base::DeviceError>(bytes)
                })
                .and_then(|result| result)
                .map_err(|error| AxVmError::device("read guest string I/O memory", error))?;
            services.write_device(&access, u64::from_le_bytes(bytes))?;
            publish_pic_interrupt_if_needed(services, vcpu_id, port)?;
        }
    }

    // Device and guest-memory work above is intentionally sleepable and runs
    // after vcpu_put(). The backend RIP and string-register update is carried as
    // an owned completion so the next bound entry commits it before injecting
    // interrupts or running the guest, matching KVM's complete_userspace_io
    // lifecycle.
    Ok(VcpuAction::Reenter(X86Completion::PortIoString(exit)))
}

fn publish_pic_interrupt_if_needed(
    services: &RunServices,
    vcpu_id: usize,
    port: Port,
) -> AxVmResult {
    let port = x86_vlapic::X86Port::new(port.number());
    if EmulatedPic::port_ranges()
        .iter()
        .any(|range| range.contains(port))
    {
        irq::publish_pic_interrupt_after_write(services, vcpu_id)?;
    }
    Ok(())
}

fn unmapped_port_value(width: AccessWidth) -> usize {
    usize::MAX >> ((core::mem::size_of::<usize>() - width.size()) * 8)
}
