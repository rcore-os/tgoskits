use core::{num::NonZeroU32, ptr::NonNull};

use crate::PerCpuRegion;

const FFI_INIT_OK: u32 = 0;
const FFI_INIT_FAILED: u32 = 1;

/// Scalar-only someboot boundary for the final relocated image.
///
/// # Safety
///
/// The three scalars must describe exclusive writable storage that remains
/// mapped until shutdown, matching [`crate::initialize_layout`].
#[doc(hidden)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __percpu_initialize_layout(
    runtime_base: usize,
    area_stride: usize,
    area_count: u32,
) -> u32 {
    let Some(runtime_base) = NonNull::new(runtime_base as *mut u8) else {
        return FFI_INIT_FAILED;
    };
    let Some(area_count) = NonZeroU32::new(area_count) else {
        return FFI_INIT_FAILED;
    };
    let region = PerCpuRegion::new(runtime_base, area_stride, area_count);
    // SAFETY: the scalar C boundary forwards its documented storage contract.
    match unsafe { crate::initialize_layout(region) } {
        Ok(_) => FFI_INIT_OK,
        Err(_) => FFI_INIT_FAILED,
    }
}
