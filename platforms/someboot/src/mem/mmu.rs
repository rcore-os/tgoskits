use kernutil::StaticCell;
use page_table_generic::PageTable;
pub use page_table_generic::{PagingError, PagingResult};

use crate::mem::ram::Ram;

pub type ArchPageTable<A> = PageTable<<crate::arch::Arch as crate::ArchTrait>::P, A>;

pub type ArchPte = <<crate::arch::Arch as crate::ArchTrait>::P as page_table_generic::TableMeta>::P;

/// Cacheability used while constructing boot-time leaf descriptors.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MemAttributes {
    /// Ordinary coherent memory.
    #[default]
    Normal,
    /// Ordinary coherent memory with a dedicated CPU-local alias.
    PerCpu,
    /// Device memory.
    Device,
    /// Uncached ordinary memory.
    Uncached,
}

/// Boot-time leaf descriptor attributes interpreted by each architecture.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PteConfig {
    pub read: bool,
    pub writable: bool,
    pub executable: bool,
    pub lower: bool,
    pub dirty: bool,
    pub global: bool,
    pub mem_attr: MemAttributes,
}

static BOOT_TABLE: StaticCell<ArchPageTable<Ram>> = StaticCell::uninit();
pub static mut BOOT_TABLE_ADDR: usize = 0;

pub(crate) fn new_boot_table() -> ArchPageTable<Ram> {
    ArchPageTable::<Ram>::new(Ram).unwrap()
}

#[cfg(any(all(efi, target_arch = "riscv64"), test))]
pub(crate) fn mapping_table_frames<T: page_table_generic::TableMeta>(
    mapping: &page_table_generic::MapConfig<PteConfig>,
) -> Option<usize> {
    use page_table_generic::Frame;

    let start = mapping.vaddr.as_usize();
    if mapping.size == 0 {
        return Some(0);
    }
    let end = start.checked_add(mapping.size)?;
    let last = end - 1;
    let mut count = 0usize;
    for level in 2..=T::LEVEL_BITS.len() {
        let span = Frame::<T, Ram>::level_size(level);
        let touched = last / span - start / span + 1;
        // Each touched entry needs a child unless it can be a block leaf.
        // A complete block at a higher level also covers only complete
        // blocks at the lower levels, so those contribute no children.
        let leaves = if mapping.allow_huge
            && level <= T::MAX_BLOCK_LEVEL
            && start % span == mapping.paddr.as_usize() % span
        {
            (end / span).saturating_sub(start.div_ceil(span))
        } else {
            0
        };
        count = count.checked_add(touched - leaves)?;
    }
    Some(count)
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

pub fn is_mmu_enabled() -> bool {
    <crate::arch::Arch as crate::ArchTrait>::is_mmu_enabled()
}

pub(crate) fn is_kernel_relocated() -> bool {
    <crate::arch::Arch as crate::ArchTrait>::is_kernel_relocated_at(
        is_kernel_relocated as *const () as usize,
    )
}

pub trait PageTableOp {
    /// 映射虚拟地址范围到物理地址范围
    fn map(&mut self, config: &page_table_generic::MapConfig<PteConfig>) -> PagingResult;

    fn unmap(&mut self, start_vaddr: page_table_generic::VirtAddr, size: usize)
    -> PagingResult<()>;
}

#[cfg(test)]
mod tests {
    use page_table_generic::{MapConfig, TableMeta, VirtAddr};

    use super::{PteConfig, mapping_table_frames};
    use crate::mem::{GB, MB, MemoryDescriptor, MemoryType, ram};

    #[derive(Clone, Copy)]
    struct ThreeLevelPages;

    impl TableMeta for ThreeLevelPages {
        type P = super::ArchPte;

        const PAGE_SIZE: usize = crate::consts::PAGE_SIZE;
        const LEVEL_BITS: &[usize] = &[9, 9, 9];
        const MAX_BLOCK_LEVEL: usize = 1;

        fn flush(_vaddr: Option<VirtAddr>) {}
    }

    #[test]
    fn boot_page_table_frames_fit_the_selected_ram_bank() {
        let page = crate::consts::PAGE_SIZE;
        let mapping = MapConfig {
            vaddr: (GB - page).into(),
            paddr: (GB - page).into(),
            size: 2 * page,
            pte: PteConfig::default(),
            allow_huge: true,
            flush: false,
        };
        // The root is separate. This mapping straddles a boundary at each of
        // the two lower levels and requires four new child tables.
        let frames = 1 + mapping_table_frames::<ThreeLevelPages>(&mapping).unwrap();
        let first = 32 * MB..42 * MB;
        let second = 64 * MB..96 * MB;
        let payload = first.len() - page;
        let map = [first.clone(), second.clone()]
            .map(|range| MemoryDescriptor::new_with_range(range, MemoryType::Free));
        let layouts = [payload, frames * page]
            .map(|bytes| core::alloc::Layout::from_size_align(bytes, page).unwrap());

        let fits = |range: &core::ops::Range<usize>| {
            let dtb = ram::planned_allocation(range.start, layouts[0], page).unwrap();
            ram::planned_allocation(dtb.end, layouts[1], page)
                .is_some_and(|table| table.end <= range.end)
        };

        assert_eq!(ram::select_range(&map, fits), Some(second));
        assert_eq!(ram::select_range(&map[..1], fits), None);
        assert_eq!(frames, 5);
    }
}
