#[macro_use]
mod _macros;

#[cfg(feature = "hv")]
#[path = "el2/mod.rs"]
mod elx;

#[cfg(not(feature = "hv"))]
#[path = "el1/mod.rs"]
mod elx;

mod addrspace;
mod context;
mod entry;
mod head;
pub mod paging;
mod power;
pub mod relocate;
mod trap;

use aarch64_cpu::registers::*;
use elx::*;
pub use paging::Entry;

use crate::{ArchTrait, arch::addrspace::PAGE_OFFSET, consts::VM_LOAD_ADDRESS, mem::PageTableInfo};

// ARM Generic Timer IRQ number (PPI 30)
const TIMER_IRQ: usize = 30;

pub struct Arch;

impl ArchTrait for Arch {
    type P = paging::Generic;

    const PAGE_OFFSET: usize = PAGE_OFFSET;

    fn post_allocator() {
        power::init();
    }

    fn kernel_code() -> &'static [u8] {
        let start = ext_sym_addr!(_head);
        let end = ext_sym_addr!(__kernel_code_end);
        let size = end - start;
        unsafe { core::slice::from_raw_parts(start as *const u8, size) }
    }

    fn is_mmu_enabled() -> bool {
        elx::is_mmu_enabled()
    }

    fn ioremap(paddr: usize, _size: usize) -> *mut u8 {
        if is_mmu_enabled() {
            todo!()
        } else {
            paddr as *mut u8
        }
    }

    fn per_cpu_trap_init(_is_primary: bool) {
        trap::setup();
        println!("Disable user page table");
        elx::set_user_table(PageTableInfo { asid: 0, addr: 0 });
        elx::flush_tlb(None);
    }

    fn systimer_irq() -> usize {
        TIMER_IRQ
    }

    fn systimer_enable() {
        elx::systick_enable();
    }

    fn systimer_irq_disable() {
        elx::systick_irq_disable();
    }

    fn systimer_set_interval(ticks: usize) {
        elx::systick_set_interval(ticks);
    }

    fn systimer_ack() {
        // ARM generic timer doesn't need explicit ACK
        // The interrupt is cleared when a new timer value is set
    }

    fn systimer_freq() -> usize {
        CNTFRQ_EL0.get() as _
    }

    fn systimer_tick() -> usize {
        CNTPCT_EL0.get() as _
    }

    fn shutdown() -> ! {
        power::shutdown()
    }

    fn irq_all_is_enabled() -> bool {
        unsafe {
            let daif: u64;
            core::arch::asm!(
                "mrs {daif}, daif",
                daif = out(reg) daif,
                options(nomem, nostack, pure)
            );
            // IRQ is enabled when bit 1 (I bit) is 0
            (daif & (1 << 1)) == 0
        }
    }

    fn irq_all_set_enable(enable: bool) {
        unsafe {
            if enable {
                core::arch::asm!("msr daifclr, #2", options(nomem, nostack));
            } else {
                core::arch::asm!("msr daifset, #2", options(nomem, nostack));
            }
        }
    }

    fn kernel_page_table() -> PageTableInfo {
        elx::get_kernal_table()
    }

    fn set_kernel_page_table(val: PageTableInfo) {
        elx::set_kernal_table(val);
        elx::flush_tlb(None);
    }

    fn user_page_table() -> PageTableInfo {
        elx::get_user_table()
    }

    fn set_user_page_table(val: PageTableInfo) {
        elx::set_user_table(val);
        elx::flush_tlb(None);
    }

    fn irq_is_enabled(_irq: crate::irq::SoftIrqId) -> bool {
        // For now, return false (can be extended with GIC support)
        false
    }

    fn irq_set_enable(_irq: crate::irq::SoftIrqId, _enable: bool) {
        // For now, do nothing (can be extended with GIC support)
    }

    fn systimer_irq_enable() {
        elx::systick_irq_enable();
    }

    fn systimer_irq_is_enabled() -> bool {
        elx::systick_irq_is_enabled()
    }

    fn enable_paging() {}

    fn virt_to_phys(vaddr: *const u8) -> usize {
        if is_mmu_enabled() {
            if vaddr as usize >= VM_LOAD_ADDRESS {
                paging::_pa(vaddr)
            } else {
                vaddr as usize & 0xffff_ffff_ffff
            }
        } else {
            vaddr as usize
        }
    }
}
