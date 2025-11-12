#[macro_use]
mod _macros;

#[cfg(feature = "hv")]
#[path = "el2/mod.rs"]
mod elx;

#[cfg(not(feature = "hv"))]
#[path = "el1/mod.rs"]
mod elx;

mod context;
mod entry;
mod head;
mod paging;
mod relocate;
mod trap;

use elx::*;

use crate::ArchTrait;

pub struct Arch;

impl ArchTrait for Arch {
    fn post_allocator() {}

    fn kernel_code() -> &'static [u8] {
        unsafe extern "C" {
            fn _head();
            fn __kernel_code_end();
        }
        let start = _head as usize;
        let end = __kernel_code_end as usize;
        unsafe { core::slice::from_raw_parts(start as *const u8, end - start) }
    }

    fn pa_bits() -> usize {
        48
    }
}
