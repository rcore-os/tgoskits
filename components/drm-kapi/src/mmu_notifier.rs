// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//! LinuxKPI storage layout for an interval MMU-notifier embedded by a DRM
//! userptr object. This provides no notifier registration or invalidation work.

use core::ffi::c_ulong;

use crate::types::{HlistNode, RbNode};

#[repr(C)]
pub struct MmuIntervalNotifier {
    pub interval_tree: IntervalTreeNode,
    pub ops: *const MmuIntervalNotifierOps,
    pub mm: *mut MmStruct,
    pub deferred_item: HlistNode,
    pub invalidate_seq: c_ulong,
}

#[repr(C)]
pub struct IntervalTreeNode {
    pub rb: RbNode,
    pub start: usize,
    pub last: usize,
    pub subtree_last: usize,
}

#[repr(C)]
pub struct MmuIntervalNotifierOps {
    pub invalidate: Option<
        unsafe extern "C" fn(*mut MmuIntervalNotifier, *const MmuNotifierRange, c_ulong) -> bool,
    >,
    pub invalidate_start: Option<
        unsafe extern "C" fn(
            *mut MmuIntervalNotifier,
            *const MmuNotifierRange,
            c_ulong,
            *mut *mut MmuIntervalNotifierFinish,
        ) -> bool,
    >,
    pub invalidate_finish: Option<unsafe extern "C" fn(*mut MmuIntervalNotifierFinish)>,
}
#[repr(C)]
pub struct MmuNotifierRange {
    _opaque: [u8; 0],
}
#[repr(C)]
pub struct MmuIntervalNotifierFinish {
    _opaque: [u8; 0],
}
pub use crate::{
    mm_native::{MmStruct, VmaIterator, current_mm, mmap_read_lock, mmap_read_unlock, vma_find},
    shmem::trylock_page,
};

const _: [(); 24] = [(); core::mem::size_of::<MmuIntervalNotifierOps>()];
const _: [(); 88] = [(); core::mem::size_of::<MmuIntervalNotifier>()];
const _: [(); 80] = [(); core::mem::offset_of!(MmuIntervalNotifier, invalidate_seq)];

const _: [(); 48] = [(); core::mem::size_of::<IntervalTreeNode>()];
const _: [(); 88] = [(); core::mem::size_of::<MmuIntervalNotifier>()];
const _: [(); 8] = [(); core::mem::align_of::<MmuIntervalNotifier>()];
const _: [(); 48] = [(); core::mem::offset_of!(MmuIntervalNotifier, ops)];
const _: [(); 56] = [(); core::mem::offset_of!(MmuIntervalNotifier, mm)];
const _: [(); 64] = [(); core::mem::offset_of!(MmuIntervalNotifier, deferred_item)];
const _: [(); 80] = [(); core::mem::offset_of!(MmuIntervalNotifier, invalidate_seq)];
