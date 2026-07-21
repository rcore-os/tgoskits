/// Initializes host-test CPU-local storage.
pub fn init() -> usize {
    #[cfg(feature = "host-test")]
    {
        host::init(4)
    }
    #[cfg(not(feature = "host-test"))]
    {
        0
    }
}

/// Returns the reserved CPU-local template size.
#[cfg(any(feature = "host-test", feature = "linked-template"))]
pub(crate) fn percpu_area_size() -> usize {
    cpu_local::cpu_area_template_size()
        .expect("CPU-area template end sentinel must follow the fixed prefix")
}

/// Rejects host execution unless the consumer selected an explicit storage
/// fixture. Merely linking source-level tests must not require kernel linker
/// symbols, while actual access must not silently invent a runtime layout.
#[cfg(not(any(feature = "host-test", feature = "linked-template")))]
pub(crate) fn percpu_area_size() -> usize {
    panic!("custom-base CPU-local access requires an explicit host-test storage fixture")
}

/// Returns the loaded template base used by relative symbol offsets.
#[doc(hidden)]
#[cfg(any(feature = "host-test", feature = "linked-template"))]
pub(crate) fn percpu_template_base() -> usize {
    cpu_local::cpu_area_template_base()
}

/// Rejects host execution unless the consumer selected an explicit storage
/// fixture; see [`percpu_area_size`].
#[doc(hidden)]
#[cfg(not(any(feature = "host-test", feature = "linked-template")))]
pub(crate) fn percpu_template_base() -> usize {
    panic!("custom-base CPU-local access requires an explicit host-test storage fixture")
}

#[cfg(feature = "host-test")]
mod host {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    static STORAGE: Mutex<Vec<u8>> = Mutex::new(Vec::new());
    static IS_INIT: AtomicBool = AtomicBool::new(false);

    pub fn init(area_count: usize) -> usize {
        if IS_INIT
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return 0;
        }
        let required_alignment = crate::required_area_alignment()
            .expect("linked CPU-local alignment metadata must be valid");
        let stride = align_up(percpu_area_size(), required_alignment)
            .expect("host CPU-local stride calculation must not overflow");
        let mut storage = STORAGE
            .lock()
            .expect("host CPU-local storage mutex must not be poisoned");
        let storage_size = stride
            .checked_mul(area_count)
            .and_then(|size| size.checked_add(required_alignment - 1))
            .expect("host CPU-local storage size must not overflow");
        storage.resize(storage_size, 0);
        let raw_base = storage.as_mut_ptr() as usize;
        let runtime_base = align_up(raw_base, required_alignment)
            .expect("host CPU-local base alignment must not overflow");
        let layout = crate::PerCpuLayoutV1 {
            runtime_base,
            area_stride: stride,
            area_count: u32::try_from(area_count).expect("host area count must fit u32"),
            flags: 0,
        };
        layout
            .validate()
            .expect("host CPU-local layout must be valid");
        // SAFETY: `storage` owns zeroed, aligned raw areas for the complete
        // process lifetime. No host thread can access them before this unique
        // typed initialization completes.
        unsafe { crate::initialize_layout(crate::PerCpuLayoutInitV2::for_supervisor_image(layout)) }
            .expect("host CPU-local layout must initialize once")
    }

    fn align_up(value: usize, alignment: usize) -> Option<usize> {
        let mask = alignment - 1;
        value.checked_add(mask).map(|aligned| aligned & !mask)
    }
}
