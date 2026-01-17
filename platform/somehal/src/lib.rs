#![no_std]
#![no_main]
#![cfg(not(any(windows, unix)))]

#[macro_use]
extern crate alloc;

#[macro_use]
extern crate log;

pub(crate) mod common;
mod driver;
pub mod irq;
mod setup;

pub use page_table_generic::{PagingError, PagingResult};
pub use setup::*;
pub use someboot::*;

#[cfg(target_arch = "loongarch64")]
#[path = "arch/loongarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "aarch64")]
#[path = "arch/aarch64/mod.rs"]
pub mod arch;

pub fn init(kernel: &'static dyn KernelOp) {
    setup::set_kernel_op(kernel);
}

pub fn post_paging() {
    driver::rdrive_setup();
}
