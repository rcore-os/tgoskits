#![no_std]
#![no_main]
#![allow(unused_features)]
#![feature(used_with_arg)]

#[macro_use]
extern crate alloc;

#[macro_use]
extern crate log;

pub(crate) mod common;
pub mod cpu;
mod driver;
pub mod irq;
pub mod rtc;
pub mod setup;

pub use page_table_generic::{PagingError, PagingResult};
pub use setup::KernelOp;
pub use someboot::{
    console, entry, fdt_addr, fdt_addr_phys, mem, power, rsdp_addr_phys, smp, timer,
};
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

#[cfg(test)]
mod tests {
    #[test]
    fn current_cpu_idx_api_is_arch_independent() {
        let current = crate::cpu::current_cpu_idx();
        let _current: Option<usize> = current;
    }
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
    arch::Plat::secondary_init_intc(meta.cpu_idx);
    arch::Plat::secondary_init_systick();

    unsafe extern "Rust" {
        fn __somehal_secondary(meta: &crate::smp::PerCpuMeta);
    }
    unsafe { __somehal_secondary(meta) };
}
