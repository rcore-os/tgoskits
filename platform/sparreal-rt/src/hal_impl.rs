use alloc::boxed::Box;
use core::time::Duration;

use somehal::{MemConfig, PageTableOp, mem::PageTableEntry};
use sparreal_kernel::{hal::al::*, impl_trait, os::mem::KernelAllocator};

struct InitImpl;

impl_trait! {
impl Platform for InitImpl {
    fn post_allocator() {
        somehal::post_allocator();
    }
    fn shutdown() -> ! {
        somehal::power::shutdown()
    }
    fn irq_is_enabled(irq: IrqId) -> bool {
        somehal::irq::irq_is_enabled(irq.into())
    }
    fn irq_set_enabled(irq: IrqId, enable: bool) {
        somehal::irq::irq_set_enable(irq.into(), enable);
    }
}
}

struct MemoryImpl;

impl_trait! {
impl Memory for MemoryImpl {
    fn virt_to_phys(virt: VirtAddr) -> PhysAddr {
        somehal::mem::virt_to_phys(virt.raw() as _).into()
    }

    fn phys_to_virt(phys: PhysAddr) -> VirtAddr {
        somehal::mem::phys_to_virt(phys.raw() as _).into()
    }

    fn page_size() -> usize {
        somehal::mem::page_size()
    }

    fn memory_map() -> &'static[ MemoryDescriptor] {
        somehal::mem::memory_map()
    }

    fn page_table_new() -> Box<dyn PageTable> {
        Box::new( PageTableImpl( somehal::mem::new_page_table(KernelAllocator)))
    }

    fn enable_paging() {
        somehal::mem::enable_paging();
    }

    fn kernel_page_table() -> PhysAddr {
        let paddr = somehal::kernel_page_table_paddr();
        PhysAddr::new(paddr)
    }

    fn set_kernel_page_table(pt: PhysAddr) {
        somehal::set_kernel_page_table_paddr(pt.raw());
    }
}
}

pub struct PageTableImpl(somehal::mem::PageTable<KernelAllocator>);

impl PageTable for PageTableImpl {
    fn addr(&self) -> PhysAddr {
        PhysAddr::new(self.0.root_paddr().raw())
    }

    fn map(
        &mut self,
        virt_start: VirtAddr,
        phys_start: PhysAddr,
        size: usize,
        settings: MemConfig,
        flush: bool,
    ) -> Result<(), PagingError> {
        let mut pte = somehal::mem::Pte::empty();
        pte.set_valid(true);
        pte.set_mem_config(settings);

        self.0.map(&somehal::mem::MapConfig {
            vaddr: virt_start.raw().into(),
            paddr: phys_start.raw().into(),
            size,
            pte,
            allow_huge: true,
            flush,
        })
    }

    fn unmap(&mut self, virt_start: VirtAddr, size: usize) -> Result<(), PagingError> {
        self.0.unmap(virt_start.raw().into(), size)
    }

    fn ioremap(
        &mut self,
        phys_start: PhysAddr,
        size: usize,
        flush: bool,
    ) -> Result<IoMemAddr, PagingError> {
        self.0
            .ioremap(phys_start.raw().into(), size, flush)
            .map(|vaddr| IoMemAddr::new(vaddr.raw()))
    }

    fn iounmap(&mut self, io_addr: IoMemAddr, size: usize) -> Result<(), PagingError> {
        self.0.iounmap(io_addr.raw().into(), size)
    }
}

struct CpuImpl;

impl_trait! {
impl Cpu for CpuImpl {
    fn current_cpu_id() -> usize {
        0 // TODO: implement
    }

    fn irq_local_is_enabled() -> bool {
        somehal::irq::irq_local_is_enabled()
    }

    fn irq_local_set_enable(enable: bool) {
        somehal::irq::irq_local_set_enable(enable);
    }


    fn systimer_irq() -> IrqId {
        somehal::irq::systimer_irq().into()
    }

    fn systimer_enable() {
        somehal::timer::enable();
    }

    fn systimer_irq_enable() {
        somehal::timer::irq_enable();
    }

    fn systimer_irq_disable() {
        somehal::timer::irq_disable();
    }

    fn systimer_irq_is_enabled() -> bool {
        somehal::timer::irq_is_enabled()
    }

    fn systimer_set_next_event(interval: Duration) {
        somehal::timer::set_next_event(interval);
    }
    fn systimer_ack() {
        somehal::timer::ack();
    }
    fn systimer_since_boot() -> Duration {
        somehal::timer::since_boot()
    }
}
}

struct ConsoleImpl;

impl_trait! {
impl Console for ConsoleImpl {
    fn early_write(bytes: &[u8]) -> usize {
        somehal::console::_write_bytes(bytes)
    }

    fn early_read() -> Option<u8> {
        None
    }
}
}

#[unsafe(no_mangle)]
pub extern "Rust" fn _somehal_handle_irq(hwirq: IrqId) {
    handle_irq(hwirq);
}
