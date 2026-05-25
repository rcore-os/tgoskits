#![no_std]
#![allow(unused_features)]
#![feature(extern_item_impls)]
#![feature(integer_cast_extras)]

extern crate alloc;
extern crate ax_driver as _;

mod interface;

use ax_api::modules::ax_log::{debug, info};
use ax_runtime::ax_app_entry;

unsafe extern "C" {
    fn runtime_entry(argc: i32, argv: *const *const u8, env: *const *const u8) -> !;
}

#[cfg(any(feature = "fs", feature = "multitask"))]
pub(crate) fn err(error: ax_errno::LinuxError) -> i32 {
    -(error as i32)
}

#[ax_app_entry]
pub fn app_entry() {
    info!("Starting application...");
    // call the runtime entry point with zeroed arguments
    const ARGC: i32 = 1;
    const NAME: &[u8; 9] = b"app_name\0";
    let argv: [*const u8; 2] = [NAME.as_ptr(), core::ptr::null()];
    let env: [*const u8; 1] = [core::ptr::null()];
    debug!("address of runtime_entry: {:p}", runtime_entry as *const ());
    unsafe {
        runtime_entry(ARGC, argv.as_ptr(), env.as_ptr());
    }
}
