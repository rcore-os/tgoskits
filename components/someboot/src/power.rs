use crate::{
    ArchTrait, DCacheOp,
    mem::{__kimage_va, __percpu, dcache_range, virt_to_phys},
    smp::PerCpuMeta,
};

pub fn shutdown() -> ! {
    crate::arch::Arch::shutdown()
}

pub fn cpu_on(cpu_idx: usize) -> Result<(), CpuOnError> {
    let entry = secondary_entry_addr();
    debug!("Secondary entry address: {entry:#x}");
    let arg = crate::smp::cpu_meta_addr(cpu_idx).ok_or(CpuOnError::InvalidParameters)?;
    debug!("Secondary entry argument (cpu meta address): {arg:#x}");

    let meta = unsafe { &*(__percpu(arg) as *const PerCpuMeta) };

    debug!("Power on CPU {meta:#x?}");
    let kimg = crate::mem::kimage_range();
    let kimg_start = __kimage_va(kimg.start);
    let size = kimg.end - kimg.start;
    dcache_range(DCacheOp::Clean, kimg_start, size);

    crate::arch::Arch::cpu_on(meta.cpu_id, entry, arg)?;
    Ok(())
}

/// secondary entry address
/// arg0 is stack top
fn secondary_entry_addr() -> usize {
    let ptr = crate::arch::Arch::secondary_entry_fn_address() as *const u8;
    virt_to_phys(ptr)
}

#[derive(thiserror::Error, Debug)]
pub enum CpuOnError {
    #[error("CPU on is not supported")]
    NotSupported,
    #[error("CPU is already on")]
    AlreadyOn,
    #[error("Invalid parameters")]
    InvalidParameters,
    #[error("Other error: {0}")]
    Other(#[from] anyhow::Error),
}
