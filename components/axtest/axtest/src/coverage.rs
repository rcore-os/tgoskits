#[cfg(all(axtest_coverage, feature = "coverage"))]
mod imp {
    use alloc::vec::Vec;
    use core::mem::ManuallyDrop;

    use crate::axtest_println;

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
    }
}

#[cfg(not(all(axtest_coverage, feature = "coverage")))]
mod imp {
    pub fn dump_coverage() {}
}

pub use imp::dump_coverage;
