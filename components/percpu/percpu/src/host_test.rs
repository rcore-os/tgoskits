//! Process-lifetime dynamic CPU areas for host-side tests.

use core::{num::NonZeroU32, ptr::NonNull};
use std::sync::Mutex;

use crate::{PerCpuError, PerCpuLayout, PerCpuRegion};

static STORAGE: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Allocates and initializes dynamic CPU areas for a host test process.
///
/// Repeated calls with the original area count return the installed layout.
/// A different count is rejected because CPU-area publication is one-shot.
pub fn initialize(area_count: NonZeroU32) -> Result<&'static PerCpuLayout, PerCpuError> {
    if let Ok(layout) = crate::layout() {
        return if layout.area_count() == area_count.get() {
            Ok(layout)
        } else {
            Err(PerCpuError::LayoutAlreadyInitialized)
        };
    }

    let mut storage = STORAGE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(layout) = crate::layout() {
        return if layout.area_count() == area_count.get() {
            Ok(layout)
        } else {
            Err(PerCpuError::LayoutAlreadyInitialized)
        };
    }

    let required_alignment = crate::required_area_alignment()?;
    let area_stride =
        align_up(crate::template_size(), required_alignment).ok_or(PerCpuError::AddressOverflow)?;
    let storage_size = area_stride
        .checked_mul(area_count.get() as usize)
        .and_then(|size| size.checked_add(required_alignment - 1))
        .ok_or(PerCpuError::AddressOverflow)?;
    storage.resize(storage_size, 0);
    let runtime_base = align_up(storage.as_mut_ptr() as usize, required_alignment)
        .ok_or(PerCpuError::AddressOverflow)?;
    let runtime_base = NonNull::new(runtime_base as *mut u8)
        .expect("aligned host storage must have a non-null address");
    let region = PerCpuRegion::new(runtime_base, area_stride, area_count);
    // SAFETY: STORAGE exclusively owns fresh, aligned raw bytes for the
    // process lifetime. No host thread can bind an area before this call
    // constructs every typed object and freezes the layout.
    unsafe { crate::initialize_layout(region) }
}

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    let mask = alignment - 1;
    value.checked_add(mask).map(|aligned| aligned & !mask)
}
