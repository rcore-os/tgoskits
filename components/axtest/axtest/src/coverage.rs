#[cfg(all(axtest_coverage, feature = "coverage"))]
mod imp {
    use alloc::vec::Vec;
    use core::{
        mem::ManuallyDrop,
        sync::atomic::{AtomicPtr, Ordering},
    };

    use crate::axtest_println;

    static WAIT_FN: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

    /// Register a wait hook invoked after the coverage profile is signalled
    /// ready. The guest busy-waits inside this hook until the host has
    /// extracted the profile via the QEMU monitor; otherwise the caller's
    /// subsequent shutdown would race the host and lose the profile.
    pub fn set_coverage_wait_fn(wait: fn()) {
        WAIT_FN.store(wait as *mut (), Ordering::Relaxed);
    }

    pub fn dump_coverage() {
        let mut coverage = Vec::new();
        match xcover::write_profraw(&mut coverage) {
            Ok(()) => {
                let coverage = ManuallyDrop::new(coverage);
                axtest_println!(
                    "AXTEST_COVERAGE status=ready addr={:p} size={}",
                    coverage.as_ptr(),
                    coverage.len()
                );
            }
            Err(err) => {
                axtest_println!("AXTEST_COVERAGE status=error reason={}", err);
            }
        }
        let ptr = WAIT_FN.load(Ordering::Relaxed);
        if !ptr.is_null() {
            // SAFETY: `ptr` was stored from a valid `fn()` pointer via
            // `set_coverage_wait_fn` and is only read back here.
            let wait: fn() = unsafe { core::mem::transmute(ptr) };
            wait();
        }
    }
}

#[cfg(not(all(axtest_coverage, feature = "coverage")))]
mod imp {
    pub fn set_coverage_wait_fn(_wait: fn()) {}
    pub fn dump_coverage() {}
}

pub use imp::{dump_coverage, set_coverage_wait_fn};
