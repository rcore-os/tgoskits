#![no_std]
#![no_main]
#![cfg(not(any(windows, unix)))]

extern crate alloc;
extern crate somehal;

pub use sparreal_kernel::entry;
pub use sparreal_kernel::*;

mod hal_impl;

#[somehal::entry]
fn main() -> ! {
    sparreal_kernel::run_kernel()
}
