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

trait ArchTrait {
    type PT<A: FrameAllocator>: PageTableOp<A>;

    fn kernel_code() -> &'static [u8];
    fn relocate_kernel_to_vm_code() -> !;
    fn post_allocator();

    fn per_cpu_trap_init(is_primary: bool);

    fn _pa(vaddr: *const u8) -> usize;
    fn _va(paddr: usize) -> *mut u8;
    fn _io(paddr: usize) -> *mut u8;
    fn ioremap(paddr: usize, size: usize) -> *mut u8;

    fn enable_paging();
    fn create_page_table<A: FrameAllocator>(allocator: A) -> Self::PT<A>;
    fn kernel_page_table() -> PageTableInfo;
    fn set_kernel_page_table(val: PageTableInfo);

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

fn prime_entry() -> ! {
    mem::set_mmu_enabled();
    mem::early_init();
    arch::Arch::per_cpu_trap_init(true);
    fdt::setup_earlycon();
    fdt::setup_memory_map();
    let _ = acpi::earlycon::acpi_setup_earlycon();
    arch::Arch::relocate_kernel_to_vm_code()
}

fn after_finally_relocate() -> ! {
    // arch::relocate_kernel_to_vm_code();
    mem::memory_map_setup();
    mem::print_memory_map();

    unsafe extern "C" {
        fn __somehal_main() -> !;
    }
    unsafe { __somehal_main() }
}

pub trait PageTableOp<A: FrameAllocator> {
    fn map(&mut self, config: &MapConfig<arch::paging::Entry>) -> Result<(), PagingError>;

    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> Result<(), PagingError>;

    fn ioremap(
        &mut self,
        phys_start: PhysAddr,
        size: usize,
        flush: bool,
    ) -> Result<VirtAddr, PagingError>;

    fn iounmap(&mut self, io_addr: VirtAddr, size: usize) -> Result<(), PagingError>;

    fn root_paddr(&self) -> PhysAddr;
}
