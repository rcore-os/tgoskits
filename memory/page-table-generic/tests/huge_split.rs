//! Break-before-make split of a huge block into a finer-granule table.
//!
//! Transparent huge pages need to split a live 2 MiB block into 512 × 4 KiB
//! leaves (on a partial unmap/protect) and, later, re-promote the same VA back to
//! a 2 MiB block. These tests cover:
//!   1. the functional round trip (map 2M -> split -> unmap -> re-promote) with no
//!      page-table-frame leak — the regression that a stranded emptied table used
//!      to block re-promotion (`AlreadyMapped`);
//!   2. splitting a *not-present* block (an `mprotect(PROT_NONE)` over a huge area)
//!      without ever freeing its data frame;
//!   3. the break-before-make ordering (clear -> flush -> install: exactly one
//!      flush, zero frees on a successful split);
//!   4. the rollback contract (a split that finds no block frees the reserved table
//!      exactly once, and never flushes, since the reservation was never installed).

#![cfg(not(target_os = "none"))]

use std::{
    alloc::{Layout, alloc, dealloc},
    sync::Mutex,
};

use page_table_generic::*;

mod mocks;

use mocks::{PteConfig, PteImpl, T4kL4, TrackedFram4k};

const HUGE_2M: usize = 0x20_0000;
const PG: usize = 0x1000;
const VA: usize = 0x40_0000;

fn map_huge(pt: &mut PageTable<T4kL4, TrackedFram4k>, vaddr: usize, paddr: usize) {
    pt.map(&MapConfig {
        vaddr: VirtAddr::from_usize(vaddr),
        paddr: PhysAddr::from_usize(paddr),
        size: HUGE_2M,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();
}

/// Functional round trip + no leak (mirrors the reference thp_remap regression).
#[test]
fn split_2m_then_unmap_then_repromote_leaves_no_frame_leaked() {
    let alloc = TrackedFram4k::default();
    let mut pt = PageTable::<T4kL4, TrackedFram4k>::new(alloc.clone()).unwrap();
    let va = VirtAddr::from_usize(VA);
    let pa = 0x1000_0000;

    // 1) a 2 MiB block; a 4 KiB map inside it conflicts — the split entry point.
    map_huge(&mut pt, VA, pa);
    assert!(
        matches!(
            pt.map(&MapConfig {
                vaddr: va,
                paddr: PhysAddr::from_usize(pa),
                size: PG,
                pte: PteImpl::kernel_mode_config(),
                allow_huge: false,
                flush: false,
            }),
            Err(PagingError::MappingConflict { .. })
        ),
        "a 4 KiB map inside a live 2 MiB block must conflict"
    );

    // 2) split -> 512 leaves auto-populated from the block's frame + flags.
    assert_eq!(pt.split_huge_page(va).unwrap(), HUGE_2M);
    for i in 0..(HUGE_2M / PG) {
        let (got, _pte) = pt.translate(va + i * PG).unwrap();
        assert_eq!(
            got.as_usize(),
            pa + i * PG,
            "leaf {i} must translate at 4 KiB granularity to the split frame"
        );
    }

    // 3) unmap the whole range: #2009 reclaims the now-empty split table inline.
    pt.unmap(va, HUGE_2M).unwrap();

    // 4) re-promote: a fresh 2 MiB block at the same VA must succeed (a stranded
    //    emptied table used to leave the L2 slot occupied -> `AlreadyMapped`).
    let pa2 = 0x2000_0000;
    map_huge(&mut pt, VA, pa2);
    let (got, _pte, level) = pt.translate_with_level(va).unwrap();
    assert_eq!(
        Frame::<T4kL4, TrackedFram4k>::level_size(level),
        HUGE_2M,
        "re-promoted mapping must be a single 2 MiB block"
    );
    assert_eq!(got.as_usize(), pa2);

    // 5) teardown: no page-table frame may leak.
    pt.unmap(va, HUGE_2M).unwrap();
    drop(pt);
    assert!(
        !alloc.has_leaks(),
        "leaked page-table frame(s) after teardown"
    );
}

/// A not-present block (PROT_NONE over a huge area) splits, and its data frame is
/// never handed to the allocator (TrackedFram4k panics on a foreign free).
#[test]
fn split_not_present_2m_block_preserves_the_data_frame() {
    let alloc = TrackedFram4k::default();
    let mut pt = PageTable::<T4kL4, TrackedFram4k>::new(alloc.clone()).unwrap();
    let va = VirtAddr::from_usize(VA);
    // A dummy data frame NOT owned by the tracking allocator: if the split ever
    // frees it, TrackedFram4k::dealloc_frame panics on the untracked paddr.
    let data = 0x1000_0000;

    map_huge(&mut pt, VA, data);
    // mprotect(PROT_NONE): default config is `valid: false` -> a not-present huge
    // block (the frame is preserved, the present bit cleared).
    pt.protect_page(va, PteConfig::default()).unwrap();
    assert_eq!(
        pt.translate(va).err(),
        Some(PagingError::NotMapped),
        "the block must now be not-present"
    );

    // peek finds the not-present block without mutating.
    let (p, _cfg, sz) = pt
        .peek_huge_block(va)
        .expect("peek must find the not-present huge block");
    assert_eq!((p.as_usize(), sz), (data, HUGE_2M));

    // split it: reached through the same `huge()` short-circuit as a present block.
    pt.split_huge_page(va)
        .expect("splitting a not-present huge block must succeed");

    // teardown: unmap clears the not-present leaves + reclaims the split table; the
    // data frame is never freed -> no foreign free, no leak.
    pt.unmap(va, HUGE_2M).unwrap();
    drop(pt);
    assert!(!alloc.has_leaks(), "leaked table frame(s) after teardown");
}

// ---- Ordering harness (same shape as tests/reclaim_flush_order.rs) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Flush,
    Dealloc(usize),
}

static OPS: Mutex<Vec<Op>> = Mutex::new(Vec::new());
static SERIALIZE: Mutex<()> = Mutex::new(());

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

/// A successful split does a single break-before-make flush and frees nothing.
#[test]
fn split_emits_one_flush_and_frees_nothing() {
    let _guard = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    OPS.lock().unwrap().clear();

    let mut pt = PageTable::<RecordingMeta, RecordingFram4k>::new(RecordingFram4k).unwrap();
    pt.map(&MapConfig {
        vaddr: VirtAddr::from_usize(VA),
        paddr: PhysAddr::from_usize(0x1000_0000),
        size: HUGE_2M,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: true,
        flush: false,
    })
    .unwrap();
    OPS.lock().unwrap().clear();

    pt.split_huge_page(VirtAddr::from_usize(VA)).unwrap();

    let ops = OPS.lock().unwrap().clone();
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::Dealloc(_))).count(),
        0,
        "a successful split installs a table and frees nothing: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::Flush)).count(),
        1,
        "exactly one break-before-make flush (clear -> flush -> install): {ops:?}"
    );
}

/// A split that finds no block (a plain 4 KiB leaf) rolls back the reserved table
/// exactly once, and never flushes (the reservation was never installed).
#[test]
fn failed_split_rolls_back_reserved_table_without_flushing() {
    let _guard = SERIALIZE.lock().unwrap_or_else(|p| p.into_inner());
    OPS.lock().unwrap().clear();

    let mut pt = PageTable::<RecordingMeta, RecordingFram4k>::new(RecordingFram4k).unwrap();
    pt.map(&MapConfig {
        vaddr: VirtAddr::from_usize(VA),
        paddr: PhysAddr::from_usize(0x1000_0000),
        size: PG,
        pte: PteImpl::kernel_mode_config(),
        allow_huge: false,
        flush: false,
    })
    .unwrap();
    OPS.lock().unwrap().clear();

    assert!(
        pt.split_huge_page(VirtAddr::from_usize(VA)).is_err(),
        "splitting a plain 4 KiB leaf must fail"
    );

    let ops = OPS.lock().unwrap().clone();
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::Dealloc(_))).count(),
        1,
        "rollback frees the reserved table exactly once: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Op::Flush)).count(),
        0,
        "an uninstalled reserved frame is never live, so no flush: {ops:?}"
    );
}
