#![no_std]
#![no_main]

extern crate somehal;

pub use sparreal_kernel::entry;
pub use sparreal_kernel::*;

mod hal_impl;

#[somehal::entry]
fn main() -> ! {
    sparreal_kernel::hal::setup::setup_allocator(somehal::mem::get_memory_map());
    somehal::post_allocator();
    sparreal_kernel::hal::setup::setup()
}
