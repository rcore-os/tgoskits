//! Task-context device exits produce owned completion records.

use axdevice_base::{BusKind, DeviceAccess, DeviceVcpuId};

use super::{ArchOps, HypercallExit, MmioReadExit, MmioWriteExit, ops::RegisterCompletion};
use crate::{
    AxVmResult,
    engine::{VcpuAction, WaitReason},
    runtime::hvc::{GuestRequest, HyperCallAbi, HyperCallOutcome},
    services::RunServices,
};

#[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
pub(crate) fn handle_mmio_read<A: ArchOps>(
    services: &RunServices,
    vcpu_id: usize,
    exit: MmioReadExit,
) -> AxVmResult<VcpuAction<A::Completion, GuestRequest>> {
    let completion = try_handle_mmio_read(services, vcpu_id, exit)?
        .ok_or_else(|| missing_mmio_error("read", exit.addr, exit.width))?;
    Ok(VcpuAction::Reenter(completion.into()))
}

pub(crate) fn try_handle_mmio_read(
    services: &RunServices,
    vcpu_id: usize,
    exit: MmioReadExit,
) -> AxVmResult<Option<RegisterCompletion>> {
    let Some(raw) = try_read_mmio_value(services, vcpu_id, exit.addr, exit.width)? else {
        return Ok(None);
    };
    let masked = raw as usize & crate::vm::width_mask(exit.width);
    let value = if exit.signed_ext {
        crate::vm::sign_extend_value(masked, exit.width)
    } else {
        masked & crate::vm::width_mask(exit.reg_width)
    };
    Ok(Some(RegisterCompletion::Gpr {
        register: exit.reg,
        value,
    }))
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn read_mmio_value(
    services: &RunServices,
    vcpu_id: usize,
    addr: axvm_types::GuestPhysAddr,
    width: axvm_types::AccessWidth,
) -> AxVmResult<usize> {
    try_read_mmio_value(services, vcpu_id, addr, width)?
        .map(|value| value as usize)
        .ok_or_else(|| missing_mmio_error("read", addr, width))
}

pub(crate) fn try_read_mmio_value(
    services: &RunServices,
    vcpu_id: usize,
    addr: axvm_types::GuestPhysAddr,
    width: axvm_types::AccessWidth,
) -> AxVmResult<Option<u64>> {
    services.read_device(&DeviceAccess::new(
        DeviceVcpuId::new(vcpu_id),
        BusKind::Mmio,
        addr.as_usize() as u64,
        width,
    ))
}

#[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
pub(crate) fn handle_mmio_write<A: ArchOps>(
    services: &RunServices,
    vcpu_id: usize,
    exit: MmioWriteExit,
) -> AxVmResult<VcpuAction<A::Completion, GuestRequest>> {
    if !try_handle_mmio_write(services, vcpu_id, exit)? {
        return Err(missing_mmio_error("write", exit.addr, exit.width));
    }
    Ok(VcpuAction::Reenter(A::Completion::default()))
}

pub(crate) fn try_handle_mmio_write(
    services: &RunServices,
    vcpu_id: usize,
    exit: MmioWriteExit,
) -> AxVmResult<bool> {
    services.write_device(
        &DeviceAccess::new(
            DeviceVcpuId::new(vcpu_id),
            BusKind::Mmio,
            exit.addr.as_usize() as u64,
            exit.width,
        ),
        exit.data,
    )
}

#[cfg(any(target_arch = "x86_64", target_arch = "loongarch64"))]
fn missing_mmio_error(
    operation: &'static str,
    addr: axvm_types::GuestPhysAddr,
    width: axvm_types::AccessWidth,
) -> crate::AxVmError {
    crate::AxVmError::device(
        "access guest MMIO",
        axdevice::DeviceManagerError::Access {
            operation,
            bus: BusKind::Mmio,
            addr: addr.as_usize() as u64,
            width,
            source: axdevice_base::DeviceError::NotFound,
        },
    )
}

pub(crate) fn handle_hypercall<A: ArchOps>(
    services: &RunServices,
    vcpu_id: usize,
    exit: HypercallExit,
    abi: HyperCallAbi,
) -> AxVmResult<VcpuAction<A::Completion, GuestRequest>> {
    match crate::runtime::hvc::handle(exit.nr, exit.args, abi, services, vcpu_id)? {
        HyperCallOutcome::Return(value) => Ok(VcpuAction::Reenter(
            RegisterCompletion::Return(value).into(),
        )),
        HyperCallOutcome::Deferred(request) => Ok(VcpuAction::Control(request)),
        HyperCallOutcome::CpuSuspendStandby { return_value } => Ok(VcpuAction::Wait(WaitReason {
            return_value: Some(return_value),
        })),
        HyperCallOutcome::CpuOff => Ok(VcpuAction::Control(GuestRequest::CpuOff {
            abi: crate::runtime::hvc::HyperCallAbi::AArch64,
        })),
        HyperCallOutcome::SystemOff => Ok(VcpuAction::Stop(crate::StopReason::SystemDown)),
        HyperCallOutcome::SystemReset => Ok(VcpuAction::Control(GuestRequest::Reset)),
        HyperCallOutcome::Unrecognized => Ok(VcpuAction::Reenter(A::Completion::default())),
    }
}
