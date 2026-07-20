//! Capability shared by vCPU backends that decode MMIO accesses completely.

use axvm_types::{AccessWidth, GuestPhysAddr};

use crate::{
    AxVmError, AxVmResult,
    architecture::{ArchOps, BoundVcpuExit},
};

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioReadExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) width: AccessWidth,
    pub(crate) reg: usize,
    pub(crate) reg_width: AccessWidth,
    pub(crate) signed_ext: bool,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct MmioWriteExit {
    pub(crate) addr: GuestPhysAddr,
    pub(crate) width: AccessWidth,
    pub(crate) data: u64,
}

/// MMIO completion for vCPU backends whose exit proves a decoded register access.
///
/// AArch64 intentionally does not include this capability: its data-abort
/// adapter must retain the complete syndrome until emulation succeeds so the
/// vCPU core can advance the guest PC or inject an architectural abort.
pub(crate) trait DecodedMmioOps: ArchOps {
    fn handle_decoded_mmio_read(
        vm: &crate::AxVMRef,
        vcpu: &crate::vm::AxVCpuRef<Self::VCpu>,
        exit: MmioReadExit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>
    where
        Self: Sized,
    {
        let raw = vm
            .get_devices()?
            .handle_mmio_read(exit.addr, exit.width)
            .map_err(|error| AxVmError::device("read guest MMIO", error))?;
        let masked = raw & width_mask(exit.width);
        let value = if exit.signed_ext {
            sign_extend_value(masked, exit.width)
        } else {
            masked & width_mask(exit.reg_width)
        };
        vcpu.set_gpr(exit.reg, value);
        Ok(BoundVcpuExit::Continue)
    }

    fn handle_decoded_mmio_write(
        vm: &crate::AxVMRef,
        exit: MmioWriteExit,
    ) -> AxVmResult<BoundVcpuExit<Self::DeferredRunWork>>
    where
        Self: Sized,
    {
        let devices = vm.get_devices()?;
        vm.dispatch_mmio_write(&devices, exit.addr, exit.width, exit.data as usize)?;
        Self::after_mmio_write(vm);
        Ok(BoundVcpuExit::Continue)
    }
}

impl<T: ArchOps> DecodedMmioOps for T {}

fn width_mask(width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => 0xff,
        AccessWidth::Word => 0xffff,
        AccessWidth::Dword => 0xffff_ffff,
        AccessWidth::Qword => usize::MAX,
    }
}

fn sign_extend_value(value: usize, width: AccessWidth) -> usize {
    match width {
        AccessWidth::Byte => (value as i8) as isize as usize,
        AccessWidth::Word => (value as i16) as isize as usize,
        AccessWidth::Dword => (value as i32) as isize as usize,
        AccessWidth::Qword => value,
    }
}
