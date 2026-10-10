//! Real table, PTE and allocator capabilities used by retirement contract tests.

use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    cell::{Cell, RefCell},
};

use page_table_generic::*;

pub(super) fn new_table() -> PageTable<RetirementMeta, RetirementAllocator> {
    PageTable::new(RetirementAllocator).unwrap()
}

pub(super) fn root_entries(
    table: &PageTable<RetirementMeta, RetirementAllocator>,
) -> &[RetirementPte] {
    // SAFETY: RetirementAllocator uses identity-mapped, aligned 512-entry
    // allocations. The table owns this root for the returned shared borrow.
    unsafe {
        core::slice::from_raw_parts(table.root_paddr().as_usize() as *const RetirementPte, 512)
    }
}

pub(super) fn reset_events() {
    EVENTS.with_borrow_mut(Vec::clear);
    WALKS.set(0);
}

pub(super) fn assert_retirement_order() {
    EVENTS.with_borrow(|events| {
        let mut pending = false;
        for event in events {
            match event {
                Event::Clear => pending = true,
                Event::Flush => pending = false,
                Event::Free => assert!(
                    !pending,
                    "retirement before completed invalidation: {events:?}"
                ),
                Event::Batch(_) => {}
            }
        }
        assert!(!pending, "unmap returned without completing invalidation");
    });
}

#[derive(Debug)]
pub(super) enum Event {
    Clear,
    Flush,
    Free,
    Batch(usize),
}

thread_local! {
    pub(super) static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    pub(super) static WALKS: Cell<usize> = const { Cell::new(0) };
    pub(super) static OBSERVED_LEAF: Cell<*const RetirementPte> = const { Cell::new(core::ptr::null()) };
    pub(super) static FLUSHED_LEAVES: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct LeafObservation;

impl LeafObservation {
    pub(super) fn new(
        table: &PageTable<RetirementMeta, RetirementAllocator>,
        address: VirtAddr,
    ) -> Self {
        let mut entries = root_entries(table).as_ptr();
        for shift in [39, 30, 21, 12] {
            let index = (address.as_usize() >> shift) & 511;
            // SAFETY: the test mapped this address through the real allocator.
            // Every non-leaf points to a live 512-entry identity-mapped table;
            // remapping a leaf never retires these tables. This observation is
            // removed before the owning PageTable is dropped, including unwind.
            let leaf = unsafe { entries.add(index) };
            let entry = unsafe { *leaf };
            if shift == 12 || entry.huge(true) {
                assert!(OBSERVED_LEAF.get().is_null());
                OBSERVED_LEAF.set(leaf);
                FLUSHED_LEAVES.with_borrow_mut(Vec::clear);
                return Self;
            }
            assert!(entry.present());
            entries = entry.paddr(true).as_usize() as *const RetirementPte;
        }
        unreachable!("the mapped address must terminate at a leaf");
    }
}

impl Drop for LeafObservation {
    fn drop(&mut self) {
        OBSERVED_LEAF.set(core::ptr::null());
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(transparent)]
pub(super) struct RetirementPte(usize);

impl PageTableEntry for RetirementPte {
    type PteConfig = usize;

    fn new_page(paddr: PhysAddr, config: usize, is_huge: bool) -> Self {
        Self(paddr.as_usize() | config | if is_huge { 2 } else { 0 })
    }

    fn new_table(paddr: PhysAddr) -> Self {
        Self(paddr.as_usize() | 1)
    }

    fn paddr(&self, _is_dir: bool) -> PhysAddr {
        (self.0 & !4095).into()
    }

    fn config(&self, _is_dir: bool) -> usize {
        self.0 & 1
    }

    fn present(&self) -> bool {
        self.0 & 1 != 0
    }

    fn huge(&self, is_dir: bool) -> bool {
        is_dir && self.0 & 2 != 0
    }

    fn unused(&self) -> bool {
        self.0 == 0
    }

    fn clear(&mut self) {
        self.0 = 0;
        EVENTS.with_borrow_mut(|events| events.push(Event::Clear));
    }
}

#[derive(Clone, Copy)]
pub(super) struct RetirementMeta;

impl TableMeta for RetirementMeta {
    type P = RetirementPte;

    const PAGE_SIZE: usize = 4096;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(_: Option<VirtAddr>) {
        EVENTS.with_borrow_mut(|events| events.push(Event::Flush));
        let leaf = OBSERVED_LEAF.get();
        if !leaf.is_null() {
            // SAFETY: LeafObservation pins the observation to this thread's
            // live page table. The walker has ended its mutable slice access
            // before calling this synchronous capability; no concurrent writer
            // or table retirement exists in the observed remap operation.
            let descriptor = unsafe { (*leaf).0 };
            FLUSHED_LEAVES.with_borrow_mut(|leaves| leaves.push(descriptor));
        }
    }

    fn flush_batch(vaddrs: &[VirtAddr]) {
        EVENTS.with_borrow_mut(|events| events.push(Event::Batch(vaddrs.len())));
        Self::flush(None);
    }
}

#[derive(Clone, Copy)]
pub(super) struct RetirementAllocator;

impl FrameAllocator for RetirementAllocator {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        // SAFETY: The nonzero layout is valid; ownership transfers to the table.
        let page = unsafe { alloc_zeroed(layout) };
        (!page.is_null()).then(|| (page as usize).into())
    }

    fn dealloc_frame(&self, frame: PhysAddr) {
        EVENTS.with_borrow_mut(|events| events.push(Event::Free));
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        // SAFETY: The real walker returns each allocated table exactly once.
        unsafe { dealloc(frame.as_usize() as *mut u8, layout) };
    }

    fn phys_to_virt(&self, frame: PhysAddr) -> *mut u8 {
        WALKS.set(WALKS.get() + 1);
        frame.as_usize() as *mut u8
    }
}
