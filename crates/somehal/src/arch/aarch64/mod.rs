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
pub use elx::Pte;
pub use elx::Pte as Entry; // 导出统一的 Entry 类型
use elx::*;
use num_align::NumAlign;
use page_table_generic::{AccessFlags, MemAttributes, MemConfig, PageTableEntry, PagingError};

use crate::{
    ArchTrait,
    consts::VM_LOAD_ADDRESS,
    mem::{PageTableInfo, page_size},
};

// ARM Generic Timer IRQ number (PPI 30)
const TIMER_IRQ: usize = 30;

pub struct PT<A: page_table_generic::FrameAllocator> {
    inner: page_table_generic::PageTable<paging::Generic, A>,
}

impl<A: page_table_generic::FrameAllocator> crate::PageTableOp<A> for PT<A> {
    fn map(
        &mut self,
        config: &page_table_generic::MapConfig<paging::Entry>,
    ) -> Result<(), page_table_generic::PagingError> {
        self.inner.map(config)
    }

    fn unmap(
        &mut self,
        virt_start: page_table_generic::VirtAddr,
        size: usize,
    ) -> Result<(), page_table_generic::PagingError> {
        self.inner.unmap(virt_start, size)
    }

    fn ioremap(
        &mut self,
        phys_start: page_table_generic::PhysAddr,
        size: usize,
        _flush: bool,
    ) -> Result<page_table_generic::VirtAddr, page_table_generic::PagingError> {
        let virt = Arch::_io(phys_start.raw());
        let end = virt as usize + size;
        let vaddr = (virt as usize).align_down(page_size());
        let paddr = phys_start.raw().align_down(page_size());
        let end = end.align_up(page_size());
        let size = end - vaddr;
        debug!(
            "ioremap: phys=0x{:x}, virt=0x{:x}, size=0x{:x}",
            paddr, vaddr, size
        );
        let config = page_table_generic::MapConfig {
            vaddr: vaddr.into(),
            paddr: paddr.into(),
            size,
            pte: {
                let mut pte = paging::Entry::new_valid();
                pte.set_mem_config(MemConfig {
                    access: AccessFlags::READ | AccessFlags::WRITE,
                    attrs: MemAttributes::Device,
                });
                pte
            },
            allow_huge: true,
            flush: true,
        };

        match self.inner.map(&config) {
            Ok(()) | Err(PagingError::MappingConflict { .. }) => {}
            Err(e) => return Err(e),
        }
        Ok(virt.into())
    }

    fn iounmap(
        &mut self,
        _io_addr: page_table_generic::VirtAddr,
        _size: usize,
    ) -> Result<(), page_table_generic::PagingError> {
        // 对于直接映射的 I/O 内存，不需要实际操作
        Ok(())
    }

    fn root_paddr(&self) -> page_table_generic::PhysAddr {
        self.inner.root_paddr()
    }
}

pub struct Arch;

impl ArchTrait for Arch {
    type PT<A: page_table_generic::FrameAllocator> = PT<A>;

    fn post_allocator() {
        power::init();
    }

    fn kernel_code() -> &'static [u8] {
        let start = ext_sym_addr!(_head);
        let end = ext_sym_addr!(__kernel_code_end);
        let size = end - start;
        unsafe { core::slice::from_raw_parts(start as *const u8, size) }
    }

    fn _va(paddr: usize) -> *mut u8 {
        (paddr as isize - crate::mem::vm_load_offset()) as usize as *mut u8
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

    fn _io(paddr: usize) -> *mut u8 {
        (paddr + addrspace::LINER_OFFSET) as *mut u8
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

    fn create_page_table<A: page_table_generic::FrameAllocator>(allocator: A) -> Self::PT<A> {
        PT {
            inner: page_table_generic::PageTable::<paging::Generic, A>::new(allocator).unwrap(),
        }
    }

    fn kernel_page_table() -> PageTableInfo {
        elx::get_kernal_table()
    }

    fn set_kernel_page_table(val: PageTableInfo) {
        elx::set_kernal_table(val);
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
