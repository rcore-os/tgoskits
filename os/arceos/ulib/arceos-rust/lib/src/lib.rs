#![no_std]
#![feature(extern_item_impls)]

extern crate alloc;

mod syscall;
mod interface;

use ax_api::modules::ax_log::{debug, info};
pub use ax_runtime;

unsafe extern "C" {
    fn runtime_entry(argc: i32, argv: *const *const u8, env: *const *const u8) -> !;
}

#[eii(ax_app_entry)]
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
