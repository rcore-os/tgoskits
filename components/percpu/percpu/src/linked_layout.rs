//! CPU-area storage described by the linked kernel image.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::{PerCpuError, PerCpuLayoutV1};

static IS_INIT: AtomicBool = AtomicBool::new(false);

fn align_up(value: usize, alignment: usize) -> Option<usize> {
    let mask = alignment - 1;
    value.checked_add(mask).map(|aligned| aligned & !mask)
}

#[cfg(feature = "host-test")]
static PERCPU_AREA_BASE: spin::once::Once<usize> = spin::once::Once::new();

unsafe extern "C" {
    fn _percpu_start();
    fn _percpu_end();
    fn _percpu_load_start();
    fn _percpu_load_end();
}

/// Initializes every statically reserved CPU-local area.
///
/// Returns the number of initialized areas, or zero when another caller has
/// already completed initialization.
pub fn init() -> usize {
    if IS_INIT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return 0;
    }

    validate_link_address_mode();
    allocate_host_areas();

    let layout = linker_layout().expect("linker-provided CPU-local layout must be valid");
    // SAFETY: the linker reserves every aligned area for the kernel lifetime.
    // Host storage is freshly zeroed; bare-metal storage has not contained a
    // published Rust value. No CPU can access an area before this call.
    unsafe { crate::initialize_layout(crate::PerCpuLayoutInitV2::for_supervisor_image(layout)) }
        .expect("CPU-local layout initialization must be unique")
}

/// Returns a typed description of the linker-reserved CPU-local layout.
pub fn linker_layout() -> Result<PerCpuLayoutV1, PerCpuError> {
    let required_alignment = crate::required_area_alignment()?;
    let area_stride =
        align_up(percpu_area_size(), required_alignment).ok_or(PerCpuError::AddressOverflow)?;
    let area_count =
        u32::try_from(linked_area_count(area_stride)).map_err(|_| PerCpuError::AddressOverflow)?;
    let layout = PerCpuLayoutV1 {
        runtime_base: runtime_area_region_base(),
        area_stride,
        area_count,
        flags: 0,
    };
    layout.validate()?;
    Ok(layout)
}

/// Returns the number of CPU-local data areas reserved by the linker.
fn linked_area_count(area_stride: usize) -> usize {
    let region_size = _percpu_end as *const () as usize - _percpu_start as *const () as usize;
    assert_eq!(
        region_size % area_stride,
        0,
        "linker-reserved CPU-local region must contain complete aligned areas"
    );
    region_size / area_stride
}

/// Returns the initialized template size for one CPU.
pub(crate) fn percpu_area_size() -> usize {
    percpu_template_end()
        .checked_sub(percpu_template_base())
        .expect("CPU-local template end must follow its prefix")
}

/// Returns the loaded template base used by relative symbol offsets.
#[doc(hidden)]
pub(crate) fn percpu_template_base() -> usize {
    _percpu_load_start as *const () as usize
}

fn percpu_template_end() -> usize {
    _percpu_load_end as *const () as usize
}

fn runtime_area_region_base() -> usize {
    #[cfg(feature = "host-test")]
    {
        *PERCPU_AREA_BASE
            .get()
            .expect("ax_percpu::init must run before host CPU-local access")
    }
    #[cfg(not(feature = "host-test"))]
    {
        _percpu_start as *const () as usize
    }
}

fn validate_link_address_mode() {
    #[cfg(not(feature = "non-zero-vma"))]
    assert_eq!(
        percpu_template_base(),
        0,
        "the per-CPU template must be linked at zero unless non-zero-vma is enabled"
    );
}

fn allocate_host_areas() {
    #[cfg(feature = "host-test")]
    {
        let total_size = _percpu_end as *const () as usize - _percpu_start as *const () as usize;
        let required_alignment = crate::required_area_alignment()
            .expect("linked CPU-local alignment metadata must be valid");
        let layout = std::alloc::Layout::from_size_align(total_size, required_alignment)
            .expect("host CPU-local allocation layout must be valid");
        PERCPU_AREA_BASE.call_once(|| {
            // SAFETY: the validated layout is intentionally leaked for the
            // complete host-test process, matching kernel CPU-area lifetime.
            unsafe { std::alloc::alloc_zeroed(layout) as usize }
        });
    }
}
