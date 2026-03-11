#![cfg(target_os = "none")]
#![no_std]

extern crate alloc;
extern crate sparreal_rt;

use core::{
    ptr::slice_from_raw_parts,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use alloc::{format, string::String, sync::Arc};

pub use bare_test_macros::tests;
pub use sparreal_rt::*;

#[cfg(feature = "net")]
pub mod net;
mod test_case;

#[sparreal_rt::entry]
fn main() -> ! {
    println!("begin test");

    for test in test_case_list() {
        println!(
            "Run test: {}{}",
            test.name,
            if test.timeout_ms > 0 {
                format!(" (timeout: {} ms)", test.timeout_ms)
            } else {
                String::new()
            }
        );
        let finished = Arc::new(AtomicBool::new(false));
        let f2 = finished.clone();
        if test.timeout_ms > 0 {
            sparreal_rt::os::time::one_shot_after(
                Duration::from_millis(test.timeout_ms),
                move || {
                    if !f2.load(Ordering::SeqCst) {
                        panic!("test {} timeout", test.name);
                    }
                },
            )
            .unwrap();
        }

        (test.test_fn)();
        finished.store(true, Ordering::SeqCst);

        println!("test {} passed", test.name);
    }

    println!("All tests passed");
}

#[repr(C)]
#[derive(Clone)]
pub struct TestCase {
    pub name: &'static str,
    pub timeout_ms: u64,
    pub test_fn: fn(),
}

fn test_case_list() -> test_case::Iter<'static> {
    unsafe extern "C" {
        fn _stest_case();
        fn _etest_case();
    }

    let data = _stest_case as *const () as usize as *const u8;
    let len = _etest_case as *const () as usize - _stest_case as *const () as usize;

    let list = test_case::ListRef::from_raw(unsafe { &*slice_from_raw_parts(data, len) });

    list.iter()
}
