#![no_std]
#![no_main]

extern crate somehal;

pub use sparreal_kernel::entry;
pub use sparreal_kernel::*;

mod hal_impl;

#[somehal::entry]
fn main() -> ! {
    somehal::println!("Starting Sparreal OS kernel...");
    let m = somehal::mem::memory_map();
    sparreal_kernel::hal::setup::setup_allocator(&m);
    somehal::post_allocator();
    sparreal_kernel::hal::setup::setup()
}
