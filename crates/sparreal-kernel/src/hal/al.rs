use core::time::Duration;

use alloc::boxed::Box;
pub use heapless::Vec as StackVec;
use kernutil::define_type;
pub use kernutil::memory::MemoryDescriptor;
pub use page_table_generic::{AccessFlags, MemAttributes, MemConfig, PagingError};

#[trait_ffi::def_extern_trait(mod_path = "hal::al")]
pub trait Memory {
    /// Convert virtual address to physical address
    fn virt_to_phys(virt: VirtAddr) -> PhysAddr;
    fn phys_to_virt(phys: PhysAddr) -> VirtAddr;
    fn page_size() -> usize;
    fn memory_map() -> &'static [MemoryDescriptor];

    fn page_table_new() -> Box<dyn PageTable>;

    fn enable_paging();
    fn kernel_page_table() -> PhysAddr;
    fn set_kernel_page_table(pt: PhysAddr);
}

#[trait_ffi::def_extern_trait(not_def_impl, mod_path = "hal::al")]
pub trait Platform {
    fn post_allocator();
    fn irq_is_enabled(irq: IrqId) -> bool;
    fn irq_set_enabled(irq: IrqId, enabled: bool);
    fn shutdown() -> !;
}

#[trait_ffi::def_extern_trait(not_def_impl, mod_path = "hal::al")]
pub trait Cpu {
    fn current_cpu_id() -> usize;
    fn irq_local_is_enabled() -> bool;
    fn irq_local_set_enable(enabled: bool);
    fn systimer_irq() -> IrqId;
    fn systimer_enable();
    fn systimer_irq_enable();
    fn systimer_irq_disable();
    fn systimer_irq_is_enabled() -> bool;
    fn systimer_set_next_event(intval: Duration);
    fn systimer_ack();
    fn systimer_since_boot() -> Duration;
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
    ) -> Result<IoMemAddr, PagingError>;

    fn iounmap(&mut self, io_addr: IoMemAddr, size: usize) -> Result<(), PagingError>;
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
