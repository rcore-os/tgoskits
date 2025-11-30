#![no_std]
#![no_main]
#![feature(iter_next_chunk)]

#[allow(unused_imports)]
#[macro_use]
extern crate alloc;

#[macro_use]
extern crate core;

#[macro_use]
extern crate log;

#[macro_use]
pub mod console;

#[cfg(target_arch = "loongarch64")]
#[path = "arch/loongarch64/mod.rs"]
pub mod arch;

#[cfg(target_arch = "aarch64")]
#[path = "arch/aarch64/mod.rs"]
pub mod arch;

mod acpi;
mod cmdline;
mod consts;
#[cfg(efi)]
mod efi_stub;
mod elf;
pub(crate) mod fdt;
pub mod irq;
pub mod mem;
pub mod power;
pub mod timer;

pub use somehal_macros::{entry, secondary_entry};

use crate::irq::SoftIrqId;

trait ArchTrait {
    fn kernel_code() -> &'static [u8];
    fn post_allocator();

    fn per_cpu_trap_init(is_primary: bool);

    fn _pa(vaddr: *const u8) -> usize;
    fn _va(paddr: usize) -> *mut u8;
    fn _io(paddr: usize) -> *mut u8;
    fn ioremap(paddr: usize, size: usize) -> *mut u8;

    fn systimer_irq() -> usize;
    fn shutdown() -> !;

    fn systimer_enable();
    fn systimer_disable();
    /// Set the timer interval in ticks
    fn systimer_set_interval(ticks: usize);
    /// Acknowledge and clear the timer interrupt
    fn systimer_ack();
    /// Get the timer frequency in Hz
    fn systimer_freq() -> usize;
    /// Get the current timer tick count
    fn systimer_tick() -> usize;

    fn irq_all_is_enabled() -> bool;
    fn irq_all_set_enable(enable: bool);

    fn irq_is_enabled(irq: SoftIrqId) -> bool;
    fn irq_set_enable(irq: SoftIrqId, enable: bool);
}

pub fn post_allocator() {
    debug!("Setup after allocator");
    arch::Arch::post_allocator();
}

fn prime_entry() -> ! {
    mem::set_mmu_enabled();
    arch::Arch::per_cpu_trap_init(true);
    fdt::setup_earlycon();
    fdt::setup_memory_map();
    let _ = acpi::earlycon::acpi_setup_earlycon();

    mem::print_memory_map();

    unsafe extern "C" {
        fn __somehal_main() -> !;
    }
    unsafe { __somehal_main() }
}
