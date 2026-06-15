use rdrive::{
    probe::OnProbeError,
    register::{FdtInfo, ProbeFdt},
};

use crate::mmio::iomap;

pub(super) fn map_first_reg(info: &FdtInfo<'_>) -> Result<core::ptr::NonNull<u8>, OnProbeError> {
    let regs = info.node.regs();
    let Some(base_reg) = regs.first() else {
        return Err(OnProbeError::other(alloc::format!(
            "[{}] has no reg",
            info.node.name()
        )));
    };

    let mmio_size = base_reg.size.unwrap_or(0x1000);
    iomap(base_reg.address as usize, mmio_size as usize)
}

pub(super) type FdtProbe<'a> = ProbeFdt<'a>;
