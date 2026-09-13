//! Exercises a real translation change across an unaligned range boundary.

use core::{alloc::Layout, ptr::NonNull};
use std::os::arceos::{
    api::{
        mem::{ax_alloc, ax_dealloc},
        modules::ax_runtime::kernel_mapping::{map_kernel_pages, unmap_kernel_range},
    },
    sync::IrqSaveGuard,
};

use ax_cpu::{
    PhysAddr, VirtAddr,
    paging::{ArchPagingMeta, DescriptorFlags, MappingFlags, PageTableEntry, Pte, TableMeta},
};
use ax_hal::{
    mem::{virt_to_phys, virtual_address_space},
    paging::PagingAllocator,
};
use someboot::PageTableRef;

const PAGE_SIZE: usize = 4096;

// The real walker writes the live test PTE, while this test explicitly owns
// the subsequent TLB operation. There is no substitute register or TLB model.
#[derive(Clone, Copy)]
struct ExplicitFlush<const GLOBAL: bool>;

impl<const GLOBAL: bool> TableMeta for ExplicitFlush<GLOBAL> {
    type P = TestEntry<GLOBAL>;
    const PAGE_SIZE: usize = ArchPagingMeta::PAGE_SIZE;
    const LEVEL_BITS: &'static [usize] = ArchPagingMeta::LEVEL_BITS;
    const MAX_BLOCK_LEVEL: usize = ArchPagingMeta::MAX_BLOCK_LEVEL;

    fn flush(_address: Option<VirtAddr>) {}
}

// Global mappings are a CPU descriptor capability, deliberately absent from
// the runtime MappingFlags policy. The test selects it through the public
// native descriptor API while retaining the production walker and encoding.
#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
struct TestEntry<const GLOBAL: bool>(Pte);

impl<const GLOBAL: bool> PageTableEntry for TestEntry<GLOBAL> {
    type PteConfig = MappingFlags;
    fn new_page(address: PhysAddr, config: MappingFlags, huge: bool) -> Self {
        let entry = Pte::new_page(address, config, huge);
        let mut flags = entry.flags();
        flags.set(DescriptorFlags::GLOBAL, GLOBAL);
        Self(Pte::from_parts(address, flags))
    }
    fn new_table(address: PhysAddr) -> Self {
        Self(Pte::new_table(address))
    }
    fn paddr(&self, is_dir: bool) -> PhysAddr {
        self.0.paddr(is_dir)
    }
    fn config(&self, is_dir: bool) -> MappingFlags {
        self.0.config(is_dir)
    }
    fn present(&self) -> bool {
        self.0.present()
    }
    fn huge(&self, is_dir: bool) -> bool {
        self.0.huge(is_dir)
    }
    fn unused(&self) -> bool {
        self.0.unused()
    }
    fn clear(&mut self) {
        self.0.clear();
    }
}

struct TestMapping {
    backing: NonNull<u8>,
    alias: Option<VirtAddr>,
}

impl TestMapping {
    fn new() -> Self {
        // SAFETY: this owner frees the same layout only after retiring aliases.
        let backing = unsafe { ax_alloc(Self::layout()) }.unwrap();
        // SAFETY: the allocation holds three writable pages for this owner.
        unsafe {
            backing.as_ptr().write_bytes(0x31, 3 * PAGE_SIZE);
        }
        Self {
            backing,
            alias: None,
        }
    }

    fn layout() -> Layout {
        Layout::from_size_align(3 * PAGE_SIZE, PAGE_SIZE).unwrap()
    }

    fn page(&self, index: usize) -> PhysAddr {
        assert!(index < 3);
        virt_to_phys(VirtAddr::from_usize(
            self.backing.as_ptr() as usize + index * PAGE_SIZE,
        ))
    }
}

impl Drop for TestMapping {
    fn drop(&mut self) {
        if let Some(alias) = self.alias {
            // Failure deliberately retains the backing allocation: freeing it
            // before synchronous translation retirement would be unsafe.
            unmap_kernel_range(alias, 2 * PAGE_SIZE).unwrap();
        }
        // SAFETY: all aliases have been synchronously retired and this is the
        // exact pointer and layout allocated by `new`.
        unsafe {
            ax_dealloc(self.backing, Self::layout());
        }
    }
}

pub(super) fn run() {
    check::<false>();
    check::<true>();
}

fn check<const GLOBAL: bool>() {
    let mut mapping = TestMapping::new();
    // SAFETY: the third page belongs exclusively to this allocation.
    unsafe {
        mapping
            .backing
            .as_ptr()
            .add(2 * PAGE_SIZE)
            .write_volatile(0x72);
    }
    let flags = MappingFlags::READ | MappingFlags::WRITE;
    let base = virtual_address_space().unwrap().kernel().start;
    let alias = map_kernel_pages(base, &[mapping.page(0), mapping.page(1)], flags).unwrap();
    mapping.alias = Some(alias);
    let second = alias + PAGE_SIZE;
    let observed;
    {
        // Keep the warmed translation and the mutation on exactly this CPU.
        let _irq = IrqSaveGuard::new();
        let _aspace = ax_mm::kernel_aspace().lock();
        // SAFETY: the active kernel root remains owned by the locked address
        // space. Its frames are mapped by PagingAllocator, and only this task
        // accesses this private alias. IRQ exclusion prevents local reentry or
        // migration during the PTE mutation and explicit invalidation.
        let mut table = unsafe {
            PageTableRef::<ExplicitFlush<GLOBAL>, PagingAllocator>::from_paddr(
                ax_cpu::mmu::read_kernel_page_table(),
                PagingAllocator,
            )
        };
        assert_eq!(table.query(second).unwrap().0, mapping.page(1));
        table.remap_page(second, mapping.page(1), flags).unwrap();
        ax_cpu::mmu::flush_tlb(Some(second));
        // SAFETY: the live alias maps the retained second backing page.
        assert_eq!(unsafe { second.as_ptr().read_volatile() }, 0x31);
        table.remap_page(second, mapping.page(2), flags).unwrap();
        if GLOBAL {
            ax_cpu::mmu::flush_tlb(None);
        } else {
            ax_cpu::mmu::flush_tlb_range(alias + PAGE_SIZE - 1, 2);
        }
        // SAFETY: both possible translations refer to retained initialized
        // pages. The observed byte distinguishes stale and updated hardware.
        observed = unsafe { second.as_ptr().read_volatile() };
        table.remap_page(second, mapping.page(1), flags).unwrap();
        ax_cpu::mmu::flush_tlb(Some(second));
    }
    drop(mapping);
    if GLOBAL {
        assert_eq!(
            observed, 0x72,
            "full TLB flush must invalidate global translations"
        );
    } else {
        assert_eq!(
            observed, 0x72,
            "unaligned TLB range must invalidate its last page"
        );
    }
}
