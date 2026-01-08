#![no_std]
#![no_main]
#![feature(iter_next_chunk)]
#![cfg(not(any(windows, unix)))]

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

#[cfg(target_arch = "x86_64")]
#[path = "arch/x86_64/mod.rs"]
pub mod arch;

mod acpi;
mod cmdline;
pub(crate) mod consts;
#[cfg(efi)]
mod efi_stub;
mod elf;
pub(crate) mod fdt;
pub mod irq;
pub mod mem;
pub mod power;
pub mod timer;

pub use page_table_generic::*;
pub use somehal_macros::{entry, secondary_entry};

use crate::{irq::SoftIrqId, mem::PageTableInfo};

#[allow(unused)]
pub trait ArchTrait {
    type P: TableGeneric;

    /// RAM 与内核虚拟地址空间的偏移
    const PAGE_OFFSET: usize;

    fn kernel_code() -> &'static [u8];
    fn post_allocator();

    fn per_cpu_trap_init(is_primary: bool);

    fn virt_to_phys(vaddr: *const u8) -> usize;
    fn ioremap(paddr: usize, size: usize) -> *mut u8;
    fn is_mmu_enabled() -> bool;

    fn enable_paging();
    // fn create_page_table<A: FrameAllocator>(allocator: A) -> Self::P<A>;
    fn kernel_page_table() -> PageTableInfo;
    fn set_kernel_page_table(val: PageTableInfo);
    fn user_page_table() -> PageTableInfo;
    fn set_user_page_table(val: PageTableInfo);

    fn systimer_irq() -> usize;
    fn shutdown() -> !;

    fn systimer_enable();
    fn systimer_irq_enable();
    fn systimer_irq_disable();
    fn systimer_irq_is_enabled() -> bool;
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
    fdt::init_with_alloc();
    debug!("Setup after allocator");
    arch::Arch::post_allocator();
}

/// Get the current kernel page table physical address and ASID
pub fn kernel_page_table_paddr() -> usize {
    arch::Arch::kernel_page_table().addr
}

/// Set the kernel page table physical address and ASID
pub fn set_kernel_page_table_paddr(paddr: usize) {
    arch::Arch::set_kernel_page_table(PageTableInfo {
        asid: 0,
        addr: paddr,
    });
}

pub fn user_page_table() -> PageTableInfo {
    arch::Arch::user_page_table()
}

pub fn set_user_page_table(pt: PageTableInfo) {
    arch::Arch::set_user_page_table(pt);
}

fn prime_entry() -> ! {
    fdt::setup_earlycon();
    let _ = acpi::earlycon::acpi_setup_earlycon();

    mem::init_after_mmu();

    arch::Arch::per_cpu_trap_init(true);

    mem::memory_map_setup();
    mem::print_memory_map();

    unsafe extern "C" {
        fn __somehal_main() -> !;
    }
    unsafe { __somehal_main() }
}
