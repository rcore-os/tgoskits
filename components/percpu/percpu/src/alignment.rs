use core::mem::align_of;
#[cfg(not(target_os = "macos"))]
use core::mem::size_of;

use cpu_local::CpuAreaPrefix;

use crate::PerCpuError;

#[cfg(not(target_os = "macos"))]
unsafe extern "C" {
    static __PERCPU_ALIGN_START: usize;
    static __PERCPU_ALIGN_END: usize;
    static __PERCPU_TEMPLATE_ALIGN_START: u8;
    static __PERCPU_TEMPLATE_ALIGN_END: u8;
}

/// Returns the maximum alignment required by the fixed prefix and every
/// macro-generated per-CPU storage object.
///
/// The descriptor table is ordinary read-only Rust data retained by the
/// linker. Keeping this calculation in the semantic layer avoids encoding an
/// architecture register or a fixed maximum alignment in either ax-percpu or
/// its proc macro.
#[cfg(not(target_os = "macos"))]
pub(crate) fn required_area_alignment() -> Result<usize, PerCpuError> {
    let start = core::ptr::addr_of!(__PERCPU_ALIGN_START) as usize;
    let end = core::ptr::addr_of!(__PERCPU_ALIGN_END) as usize;
    let byte_len = end
        .checked_sub(start)
        .ok_or(PerCpuError::MalformedAlignmentMetadata { start, end })?;
    // An empty output section may be placed at an arbitrary byte address by
    // the host linker. There is no descriptor to read in that case, so word
    // alignment is required only for a non-empty descriptor table.
    if !byte_len.is_multiple_of(size_of::<usize>())
        || (byte_len != 0 && !start.is_multiple_of(align_of::<usize>()))
    {
        return Err(PerCpuError::MalformedAlignmentMetadata { start, end });
    }

    let mut required = align_of::<CpuAreaPrefix>();
    let mut descriptor_address = start;
    while descriptor_address < end {
        // SAFETY: the linker boundaries were checked for order, word size,
        // and word alignment. The retained section consists exclusively of
        // immutable `usize` descriptors emitted by `def_percpu`.
        let alignment = unsafe { (descriptor_address as *const usize).read() };
        if alignment == 0 || !alignment.is_power_of_two() {
            return Err(PerCpuError::InvalidSymbolAlignment(alignment));
        }
        required = required.max(alignment);
        descriptor_address += size_of::<usize>();
    }
    let linker_start = core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_START) as usize;
    let linker_end = core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_END) as usize;
    let linker_required =
        linker_end
            .checked_sub(linker_start)
            .ok_or(PerCpuError::MalformedAlignmentMetadata {
                start: linker_start,
                end: linker_end,
            })?;
    if linker_required != required {
        return Err(PerCpuError::AlignmentMetadataMismatch {
            descriptors: required,
            linker: linker_required,
        });
    }
    Ok(required)
}

/// macOS does not expose the ELF linker-section contract used by dynamic CPU
/// areas. This fallback permits source-only consumers but not area creation.
#[cfg(target_os = "macos")]
pub(crate) fn required_area_alignment() -> Result<usize, PerCpuError> {
    Ok(align_of::<CpuAreaPrefix>())
}
