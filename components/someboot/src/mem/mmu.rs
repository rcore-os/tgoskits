use kernutil::StaticCell;
use page_table_generic::PageTable;
pub use page_table_generic::{PagingError, PagingResult};

use crate::mem::ram::Ram;

pub type ArchPageTable<A> = PageTable<<crate::arch::Arch as crate::ArchTrait>::P, A>;

pub type ArchPte = <<crate::arch::Arch as crate::ArchTrait>::P as page_table_generic::TableMeta>::P;

static BOOT_TABLE: StaticCell<ArchPageTable<Ram>> = StaticCell::uninit();
pub static mut BOOT_TABLE_ADDR: usize = 0;
static mut MMU_ENABLED: bool = false;

pub(crate) fn new_boot_table() -> ArchPageTable<Ram> {
    ArchPageTable::<Ram>::new(Ram).unwrap()
}

pub fn new_page_table<A: page_table_generic::FrameAllocator>(
    allocator: A,
) -> Result<ArchPageTable<A>, PagingError> {
    ArchPageTable::<A>::new(allocator)
}

pub(crate) fn set_boot_table(table: ArchPageTable<Ram>) {
    let root_addr: usize = table.root_paddr().into();
    // aarch64 `LDXR` `LDAXR` not work here before MMU is enabled
    unsafe {
        BOOT_TABLE_ADDR = root_addr;
        BOOT_TABLE.init_single_core(table)
    };
}

pub(crate) fn boot_table_addr() -> usize {
    unsafe { BOOT_TABLE_ADDR }
}

pub(crate) fn is_mmu_enabled() -> bool {
    unsafe { MMU_ENABLED }
}

pub(crate) fn set_mmu_enabled() {
    unsafe { MMU_ENABLED = true };
}

pub trait PageTableOp {
    /// 映射虚拟地址范围到物理地址范围
    fn map(&mut self, config: &page_table_generic::MapConfig) -> PagingResult;

    fn unmap(&mut self, start_vaddr: page_table_generic::VirtAddr, size: usize)
    -> PagingResult<()>;
}
