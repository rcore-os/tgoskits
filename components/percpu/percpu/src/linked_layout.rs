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
    copy_template_to_primary_host_area(&layout);
    copy_template_to_secondary_areas(&layout);
    // SAFETY: the linker reserves every area for the kernel lifetime and the
    // initialization above copies the complete linked template before publish.
    unsafe { crate::install_layout(layout) }.expect("CPU-local layout installation must be unique");
    layout.area_count as usize
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
    percpu_link_end().wrapping_sub(percpu_link_base())
}

/// Returns the link-time base used by per-CPU symbol relocation.
#[doc(hidden)]
pub(crate) fn percpu_link_base() -> usize {
    _percpu_load_start as *const () as usize
}

fn percpu_link_end() -> usize {
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
        percpu_link_base(),
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

fn copy_template_to_primary_host_area(layout: &PerCpuLayoutV1) {
    #[cfg(feature = "host-test")]
    {
        // SAFETY: the host linker script maps the initialized template at its
        // link address. The freshly allocated primary area is non-overlapping,
        // writable, and remains live for the test process.
        unsafe {
            core::ptr::copy_nonoverlapping(
                percpu_link_base() as *const u8,
                layout.runtime_base as *mut u8,
                percpu_area_size(),
            );
        }
    }
    #[cfg(not(feature = "host-test"))]
    let _ = layout;
}

fn copy_template_to_secondary_areas(layout: &PerCpuLayoutV1) {
    let primary_base = layout.runtime_base;
    let area_size = percpu_area_size();
    for cpu_id in 1..layout.area_count as usize {
        let secondary_base = primary_base + cpu_id * layout.area_stride;
        #[cfg(not(feature = "host-test"))]
        assert!(secondary_base + area_size <= _percpu_end as *const () as usize);
        // SAFETY: linker_layout validates non-overlapping areas. Startup owns
        // every secondary area exclusively and the primary holds the template.
        unsafe {
            core::ptr::copy_nonoverlapping(
                primary_base as *const u8,
                secondary_base as *mut u8,
                area_size,
            );
        }
    }
}
