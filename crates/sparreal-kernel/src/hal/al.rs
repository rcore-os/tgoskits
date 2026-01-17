use core::{ptr::NonNull, time::Duration};

use alloc::boxed::Box;
pub use heapless::Vec as StackVec;
use kernutil::define_type;
pub use kernutil::memory::{MemoryDescriptor, PageTableInfo};
pub use page_table_generic::{AccessFlags, MemAttributes, MemConfig, PagingError};
pub use rdrive::register::DriverRegisterSlice;

use crate::os::mem::{__io, page_size};

#[trait_ffi::def_extern_trait(mod_path = "hal::al")]
pub trait Memory {
    fn _va(paddr: PhysAddr) -> VirtAddr;
    fn _io(paddr: PhysAddr) -> VirtAddr;

    /// 内核镜像在虚拟地址空间中的偏移
    fn kimage_offset() -> isize;

    /// Convert virtual address to physical address
    fn virt_to_phys(virt: VirtAddr) -> PhysAddr;

    fn page_size() -> usize;
    fn memory_map() -> &'static [MemoryDescriptor];

    fn page_table_new() -> Result<Box<dyn PageTable>, PagingError>;

    fn kernel_page_table() -> PhysAddr;
    fn set_kernel_page_table(pt: PhysAddr);
    fn user_page_table() -> PageTableInfo;
    fn set_user_page_table(pt: PageTableInfo);
}

#[trait_ffi::def_extern_trait(not_def_impl, mod_path = "hal::al")]
pub trait Platform {
    fn post_allocator();
    fn irq_is_enabled(irq: IrqId) -> bool;
    fn irq_set_enabled(irq: IrqId, enabled: bool);

    fn shutdown() -> !;

    fn fdt_addr() -> Option<NonNull<u8>>;
    fn post_paging();
}

#[trait_ffi::def_extern_trait(not_def_impl, mod_path = "hal::al")]
pub trait Cpu {
    fn current_cpu_id() -> usize;
    fn irq_local_is_enabled() -> bool;
    fn irq_local_set_enable(enabled: bool);
    fn systick_irq_id() -> IrqId;
    fn systick_enable();
    fn systick_irq_enable();
    fn systick_irq_disable();
    fn systick_irq_is_enabled() -> bool;
    // fn systimer_set_next_event(intval: Duration);
    fn systick_ack();
    fn systick_frequency() -> usize;
    // fn systimer_since_boot() -> Duration;
    fn systick_ticks() -> usize;
    /// Set next irq interval in ticks
    fn systick_set_interval(ticks: usize);
}

#[trait_ffi::def_extern_trait(mod_path = "hal::al", not_def_impl)]
pub trait Console {
    fn early_write(bytes: &[u8]) -> usize;
    fn early_read() -> Option<u8>;
}

pub fn handle_irq(irq: IrqId) {
    crate::os::irq::handle_irq(irq);
}

pub trait PageTable: Send + 'static {
    fn addr(&self) -> PhysAddr;
    fn map(
        &mut self,
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        size: usize,
        settings: MemConfig,
        flush: bool,
    ) -> Result<(), PagingError>;
    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> Result<(), PagingError>;

    fn ioremap(
        &mut self,
        phys_start: PhysAddr,
        size: usize,
        flush: bool,
    ) -> Result<IoMemAddr, PagingError> {
        let virt = __io(phys_start);
        let end = virt + size;
        let vaddr = virt.align_down(page_size());
        let paddr = phys_start.align_down(page_size());
        let end = end.align_up(page_size());
        let size = end - vaddr;
        debug!("ioremap: phys={}, virt={}, size=0x{:x}", paddr, vaddr, size);
        let settings = MemConfig {
            access: AccessFlags::READ | AccessFlags::WRITE,
            attrs: MemAttributes::Device,
        };

        self.map(
            vaddr.raw().into(),
            paddr.raw().into(),
            size,
            settings,
            flush,
        )?;

        Ok(virt.raw().into())
    }
}

define_type! {
    /// Interrupt Request Identifier
    IrqId(usize, "{:#x}"),
    /// Physical Address
    PhysAddr(usize, "{:#x}"),
    /// Virtual Address
    VirtAddr(usize, "{:#x}"),
    /// I/O Memory Address
    IoMemAddr(usize, "{:#x}"),
    ///
    Asid(usize, "{:#x}"),
}
