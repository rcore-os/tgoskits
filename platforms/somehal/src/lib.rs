#![no_std]
#![no_main]
#![allow(unused_features)]
#![feature(used_with_arg)]
#![cfg(not(any(windows, unix)))]

#[macro_use]
extern crate alloc;

#[macro_use]
extern crate log;

pub(crate) mod common;
mod driver;
pub mod irq;
pub mod setup;

pub use page_table_generic::{PagingError, PagingResult};
pub use setup::KernelOp;
pub use someboot::*;
pub use somehal_macros::somehal_secondary_entry as secondary_entry;

use crate::common::PlatOp;

#[cfg(target_arch = "loongarch64")]
#[path = "arch/loongarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "aarch64")]
#[path = "arch/aarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "x86_64")]
#[path = "arch/x86_64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "riscv64")]
#[path = "arch/riscv64/mod.rs"]
pub mod arch;

pub fn init(kernel: &'static dyn KernelOp) {
    setup::set_kernel_op(kernel);
}

pub fn post_paging() {
    someboot::post_allocator();
    // note: irq controller should be initialized when probe.
    driver::rdrive_setup();
}

#[unsafe(no_mangle)]
pub fn __somehal_secondary_default() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

#[someboot::secondary_entry]
fn secondary_entry() -> ! {
    someboot::set_kernel_page_table_paddr(meta.primary_table_paddr);
    arch::Plat::secondary_init();
    arch::Plat::secondary_init_intc();
    arch::Plat::secondary_init_systick();

    unsafe extern "Rust" {
        fn __somehal_secondary(meta: &crate::smp::PerCpuMeta);
    }
    unsafe { __somehal_secondary(meta) };
}
