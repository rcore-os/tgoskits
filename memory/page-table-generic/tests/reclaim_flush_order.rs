//! Break-before-free ordering for intermediate page-table reclaim.
//!
//! When `unmap` empties an intermediate table it returns that frame to the
//! allocator. The frame's translation must be invalidated from the TLB *before*
//! it is freed: otherwise a concurrent core can reallocate and overwrite the
//! frame while a stale table-walk still reads it as page-table entries, mapping
//! to arbitrary physical memory. These tests record `flush` and `dealloc_frame`
//! into one ordered log and assert every reclaimed frame is flushed before it is
//! freed (and that `flush: false` suppresses the flush while still reclaiming).

#![cfg(not(target_os = "none"))]

use std::{
    alloc::{Layout, alloc, dealloc},
    sync::Mutex,
};

use page_table_generic::*;

mod mocks;

use mocks::PteImpl;

/// One entry in the ordered operation log shared by the recording `TableMeta`
/// and `FrameAllocator` below.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Flush,
    Dealloc(usize),
}

/// `TableMeta::flush` is a static method with no instance state, so the log has
/// to be a static. `SERIALIZE` keeps the two tests from interleaving on it.
static OPS: Mutex<Vec<Op>> = Mutex::new(Vec::new());
static SERIALIZE: Mutex<()> = Mutex::new(());

/// 4-level table so a single 4 KiB unmap reclaims a *chain* of intermediate
/// tables (L1, L2, L3) — the chained reclaim is what exposes the missing flush
/// as back-to-back `Dealloc`s with no `Flush` between them.
#[derive(Clone, Copy)]
struct RecordingMeta;

impl TableMeta for RecordingMeta {
    type P = PteImpl;

    const PAGE_SIZE: usize = 0x1000;
    const LEVEL_BITS: &[usize] = &[9, 9, 9, 9];
    const MAX_BLOCK_LEVEL: usize = 3;

    fn flush(vaddr: Option<VirtAddr>) {
        if vaddr.is_some() {
            OPS.lock().unwrap().push(Op::Flush);
        }
    }
}

/// Real-backing allocator (so page walks are valid and nothing leaks) that also
/// records every `dealloc_frame` into the shared ordered log.
#[derive(Clone, Copy)]
struct RecordingFram4k;

impl FrameAllocator for RecordingFram4k {
    fn alloc_frame(&self) -> Option<PhysAddr> {
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            None
        } else {
            Some(PhysAddr::from_usize(ptr as usize))
        }
    }

    fn dealloc_frame(&self, frame: PhysAddr) {
        OPS.lock().unwrap().push(Op::Dealloc(frame.as_usize()));
        let layout = Layout::from_size_align(4096, 4096).unwrap();
        unsafe { dealloc(frame.as_usize() as *mut u8, layout) };
    }

    fn phys_to_virt(&self, paddr: PhysAddr) -> *mut u8 {
        paddr.as_usize() as *mut u8
    }
}

/// Map a single 4 KiB page, then reset the log and record only the unmap.
fn map_one_page_then_reset() -> PageTable<RecordingMeta, RecordingFram4k> {
    let mut page_table = PageTable::<RecordingMeta, RecordingFram4k>::new(RecordingFram4k).unwrap();
    page_table
        .map(&MapConfig {
            vaddr: VirtAddr::from_usize(0x20_0000),
            paddr: PhysAddr::from_usize(0x20_0000),
            size: RecordingMeta::PAGE_SIZE,
            pte: PteImpl::kernel_mode_config(),
            allow_huge: false,
            flush: false,
        })
        .unwrap();
    OPS.lock().unwrap().clear();
    page_table
}

#[test]
fn reclaimed_table_is_flushed_before_it_is_freed() {
    let _guard = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    OPS.lock().unwrap().clear();

    let mut page_table = map_one_page_then_reset();
    page_table
        .unmap(VirtAddr::from_usize(0x20_0000), RecordingMeta::PAGE_SIZE)
        .unwrap();

    let ops = OPS.lock().unwrap().clone();
    let deallocs = ops.iter().filter(|o| matches!(o, Op::Dealloc(_))).count();
    assert!(
        deallocs >= 2,
        "expected the single-page unmap to reclaim a chain of >=2 intermediate tables (otherwise \
         this test cannot detect the ordering bug), got {deallocs}: {ops:?}"
    );

    // Break-before-free: every reclaimed frame must be flushed immediately
    // before it is freed. Without the fix the chained reclaims emit back-to-back
    // `Dealloc`s, so a `Dealloc` is preceded by another `Dealloc`, not a `Flush`.
    for (i, op) in ops.iter().enumerate() {
        if matches!(op, Op::Dealloc(_)) {
            assert!(
                i > 0 && ops[i - 1] == Op::Flush,
                "intermediate table freed without a preceding TLB flush (break-before-free \
                 violated) at op {i}: {ops:?}"
            );
        }
    }
}

#[test]
fn reclaim_with_flush_disabled_emits_no_flush() {
    let _guard = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    OPS.lock().unwrap().clear();

    let mut page_table = map_one_page_then_reset();
    page_table
        .unmap_with_config(&UnmapConfig {
            start_vaddr: VirtAddr::from_usize(0x20_0000),
            size: RecordingMeta::PAGE_SIZE,
            flush: false,
        })
        .unwrap();

    let ops = OPS.lock().unwrap().clone();
    let deallocs = ops.iter().filter(|o| matches!(o, Op::Dealloc(_))).count();
    let flushes = ops.iter().filter(|o| matches!(o, Op::Flush)).count();
    assert!(
        deallocs >= 2,
        "reclaim must still free the chained tables when flush is disabled: {ops:?}"
    );
    assert_eq!(
        flushes, 0,
        "no TLB flush must be emitted when flush is disabled: {ops:?}"
    );
}
