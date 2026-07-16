/// Returns the loaded address of the fixed CPU-area template prefix.
///
/// All production kernels are position-independent images loaded through
/// someboot. Letting rustc materialize the symbol keeps this operation valid
/// after the loader's relative relocations on every architecture. Callers use
/// this address only as the origin of a template-relative range; it is never a
/// CPU-local runtime area address.
#[inline(always)]
pub fn cpu_area_template_base() -> usize {
    core::ptr::addr_of!(crate::__AX_CPU_AREA_PREFIX).cast::<u8>() as usize
}

/// Returns the exact initialized CPU-area template size.
///
/// The final sentinel is one byte wide. Both boundaries belong to the same
/// loaded image, so their checked difference is independent of the address at
/// which someboot placed that image.
#[inline(always)]
pub fn cpu_area_template_size() -> Option<usize> {
    let start = cpu_area_template_base();
    let end = core::ptr::addr_of!(crate::__AX_CPU_AREA_TEMPLATE_END) as usize;
    end.checked_sub(start)?
        .checked_add(core::mem::size_of::<u8>())
}
