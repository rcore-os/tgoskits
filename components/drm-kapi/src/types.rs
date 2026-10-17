// SPDX-License-Identifier: MIT
// Copyright 2026 TheKernel contributors. See ../LICENSE-MIT.
//
// Linux kernel structure layouts shared by the generic kernel-API shims and by
// the DRM translations that embed them. Each record keeps the C layout of
// the Linux v7.2.3 x86_64 configuration; the layout assertions travel with the
// records. These are pure kernel records with no GPU-driver dependency.

#![allow(non_camel_case_types)]

use core::{
    ffi::{c_char, c_uint, c_ulong, c_void},
    mem::ManuallyDrop,
};

// `struct mutex` (include/linux/mutex_types.h): opaque 24-byte x86_64 record.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct Mutex {
    _opaque: [u8; 24],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ListHead {
    pub next: *mut ListHead,
    pub prev: *mut ListHead,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LlistNode {
    pub next: *mut LlistNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LlistHead {
    pub first: *mut LlistNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HlistNode {
    pub next: *mut HlistNode,
    pub pprev: *mut *mut HlistNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct HlistHead {
    pub first: *mut HlistNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RbNode {
    pub parent_color: usize,
    pub right: *mut RbNode,
    pub left: *mut RbNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RbRoot {
    pub node: *mut RbNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RbRootCached {
    pub root: RbRoot,
    pub leftmost: *mut RbNode,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TimerList {
    pub entry: HlistNode,
    pub expires: c_ulong,
    pub function: Option<unsafe extern "C" fn(*mut TimerList)>,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WorkStruct {
    pub data: c_ulong,
    pub entry: ListHead,
    pub function: Option<unsafe extern "C" fn(*mut WorkStruct)>,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DelayedWork {
    pub work: WorkStruct,
    pub timer: TimerList,
    pub workqueue: *mut c_void,
    pub cpu: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AtomicT {
    pub counter: i32,
}

/// `struct atomic64_t` (include/linux/atomic/atomic-long.h): one s64 counter.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct AtomicT64 {
    pub counter: i64,
}
const _: [(); 8] = [(); core::mem::size_of::<AtomicT64>()];

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Seqcount {
    pub sequence: u32,
}

// CONFIG_LOCKDEP and CONFIG_PREEMPT_RT are disabled in the source kernel
// configuration used for this translation, so seqcount_mutex_t is seqcount_t.
pub type SeqcountMutex = Seqcount;

/// `rwlock_t` storage for the non-RT, non-debug x86_64 configuration
/// (asserted against the reference DWARF size of 8 bytes, align 4).
#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct Rwlock {
    _opaque: [u8; 8],
}
const _: [(); 8] = [(); core::mem::size_of::<Rwlock>()];

/// `seqlock_t` (include/linux/seqlock_types.h): a seqcount plus a spinlock.
#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct Seqlock {
    pub seqcount: Seqcount,
    pub lock: Spinlock,
}
const _: [(); 8] = [(); core::mem::size_of::<Seqlock>()];

#[repr(C, align(4))]
#[derive(Clone, Copy)]
pub struct Spinlock {
    _opaque: [u8; 4],
}

impl Spinlock {
    pub const fn new() -> Self {
        Self { _opaque: [0; 4] }
    }
}

impl Default for Spinlock {
    fn default() -> Self {
        Self::new()
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AtomicNotifierHead {
    pub lock: Spinlock,
    pub head: *mut c_void,
}

#[repr(C)]
pub struct RefcountT {
    pub refs: AtomicT,
}

#[repr(C)]
pub struct Kref {
    pub refcount: RefcountT,
}

#[repr(C, align(8))]
pub struct RcuHead {
    pub next: *mut RcuHead,
    pub func: Option<unsafe extern "C" fn(*mut RcuHead)>,
}

#[repr(C)]
pub union DmaFenceLock {
    pub extern_lock: *mut Spinlock,
    pub inline_lock: Spinlock,
}

#[repr(C)]
pub union DmaFenceTimestamp {
    pub cb_list: ManuallyDrop<ListHead>,
    pub timestamp: i64,
    pub rcu: ManuallyDrop<RcuHead>,
}

#[repr(C)]
pub struct DmaFence {
    pub lock: DmaFenceLock,
    pub ops: *const c_void,
    pub timestamp_union: DmaFenceTimestamp,
    pub context: u64,
    pub seqno: u64,
    pub flags: c_ulong,
    pub refcount: Kref,
    pub error: i32,
}

impl core::ops::Deref for DmaFence {
    type Target = DmaFenceTimestamp;

    fn deref(&self) -> &Self::Target {
        &self.timestamp_union
    }
}

#[repr(C)]
pub struct DmaFenceCb {
    pub node: ListHead,
    pub func: Option<unsafe extern "C" fn(*mut DmaFence, *mut DmaFenceCb)>,
}

#[repr(C)]
pub struct DmaFenceChain {
    pub base: DmaFence,
    pub prev: *mut DmaFence,
    pub prev_seqno: u64,
    pub fence: *mut DmaFence,
    // Linux overlays the chain callback and irq_work at this exact offset.
    pub work: IrqWork,
}
const _: [(); 120] = [(); core::mem::size_of::<DmaFenceChain>()];
const _: [(); 88] = [(); core::mem::offset_of!(DmaFenceChain, work)];

/// Layout of `struct dma_fence_ops` (include/linux/dma-fence.h), first two hooks
/// used by this file; the remaining hooks are reserved and left NULL.
#[repr(C)]
pub struct DmaFenceOps {
    pub get_driver_name: Option<unsafe extern "C" fn(*mut DmaFence) -> *const c_char>,
    pub get_timeline_name: Option<unsafe extern "C" fn(*mut DmaFence) -> *const c_char>,
    pub enable_signaling: Option<unsafe extern "C" fn(*mut DmaFence) -> bool>,
    pub signaled: Option<unsafe extern "C" fn(*mut DmaFence) -> bool>,
    pub wait: Option<unsafe extern "C" fn(*mut DmaFence, bool, i64) -> i64>,
    pub release: Option<unsafe extern "C" fn(*mut DmaFence)>,
    pub set_deadline: Option<unsafe extern "C" fn(*mut DmaFence, i64)>,
}

// Linux v7.2.3 include/linux/dma-resv.h `struct dma_resv`: the embedded WW mutex
// and the native reservation-sidecar pointer.
#[repr(C, align(8))]
pub struct DmaResv {
    pub lock: crate::ww_mutex::WwMutex,
    pub fences: *mut c_void,
}

const _: () = assert!(core::mem::size_of::<DmaResv>() == 40);
const _: () = assert!(core::mem::align_of::<DmaResv>() == 8);
const _: () = assert!(core::mem::offset_of!(DmaResv, lock) == 0);
const _: () = assert!(core::mem::size_of::<crate::ww_mutex::WwMutex>() == 32);
const _: () = assert!(core::mem::offset_of!(DmaResv, fences) == 32);

#[repr(C)]
pub struct WaitQueueHead {
    pub lock: Spinlock,
    pub head: ListHead,
}

pub use crate::xarray::XArray;
const _: [(); 16] = [(); core::mem::size_of::<XArray>()];

// Linux v7.2.3 framework records embedded by value in DRM driver
// objects. Their storage is kernel-owned; only their ABI size/alignment
// is needed by this source file.
#[repr(C, align(8))]
pub struct DrmMmNode {
    pub color: c_ulong,
    pub start: u64,
    pub size: u64,
    pub mm: *mut DrmMm,
    pub node_list: ListHead,
    pub hole_stack: ListHead,
    pub rb: RbNode,
    pub rb_hole_size: RbNode,
    pub rb_hole_addr: RbNode,
    pub subtree_last: u64,
    pub hole_size: u64,
    pub subtree_max_hole: u64,
    pub flags: c_ulong,
}
const _: [(); 168] = [(); core::mem::size_of::<DrmMmNode>()];
const _: [(); 160] = [(); core::mem::offset_of!(DrmMmNode, flags)];

#[repr(C, align(8))]
pub struct IrqWork {
    pub node: IrqWorkNode,
    pub func: Option<unsafe extern "C" fn(*mut IrqWork)>,
    pub irqwait: *mut c_void,
}

#[repr(C)]
pub struct IrqWorkNode {
    pub next: *mut IrqWorkNode,
    pub flags: AtomicT,
    pub src: u16,
    pub dst: u16,
}

#[repr(C, align(8))]
pub struct Hrtimer {
    _opaque: [u8; 80],
}

#[repr(C, align(8))]
pub struct WaitQueueEntry {
    pub flags: u32,
    pub private: *mut c_void,
    pub func: Option<unsafe extern "C" fn(*mut WaitQueueEntry, u32, i32, *mut c_void) -> i32>,
    pub entry: ListHead,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PinCookie;

#[repr(C)]
pub struct SgTable {
    pub sgl: *mut Scatterlist,
    pub nents: u32,
    pub orig_nents: u32,
}
const _: [(); 16] = [(); core::mem::size_of::<SgTable>()];
const _: [(); 0] = [(); core::mem::offset_of!(SgTable, sgl)];
const _: [(); 8] = [(); core::mem::offset_of!(SgTable, nents)];

#[repr(C)]
pub struct RadixTreeRoot {
    pub height: u32,
    pub gfp_mask: u32,
    pub rnode: *mut c_void,
}
const _: [(); 16] = [(); core::mem::size_of::<RadixTreeRoot>()];

/// Source `struct drm_mm` from `include/drm/drm_mm.h`.
#[repr(C)]
pub struct DrmMm {
    pub color_adjust: Option<unsafe extern "C" fn(*const DrmMmNode, c_ulong, *mut u64, *mut u64)>,
    pub hole_stack: ListHead,
    pub head_node: DrmMmNode,
    pub interval_tree: RbRootCached,
    pub holes_size: RbRootCached,
    pub holes_addr: RbRoot,
    pub scan_active: c_ulong,
}

#[repr(C)]
pub struct Scatterlist {
    pub page_link: usize,
    pub offset: u32,
    pub length: u32,
    pub dma_address: u64,
    pub dma_length: u32,
    pub _dma_flags_or_padding: u32,
}
const _: [(); 32] = [(); size_of::<Scatterlist>()];
const _: [(); 16] = [(); core::mem::offset_of!(Scatterlist, dma_address)];
const _: [(); 24] = [(); core::mem::offset_of!(Scatterlist, dma_length)];

#[repr(C)]
pub union TaskletCallbacks {
    pub func: Option<unsafe extern "C" fn(c_ulong)>,
    pub callback: Option<unsafe extern "C" fn(*mut TaskletStruct)>,
}

/// `struct tasklet_struct` from linux/interrupt.h, embedded by the scheduler
/// engine header.
#[repr(C)]
pub struct TaskletStruct {
    pub next: *mut TaskletStruct,
    pub state: c_ulong,
    pub count: AtomicT,
    pub use_callback: bool,
    pub callbacks: TaskletCallbacks,
    pub data: c_ulong,
}

#[repr(C)]
pub struct Page {
    _opaque: [u8; 0],
}

// Linux forward declarations used only behind pointers in this header.
#[repr(C, align(8))]
pub struct File {
    pub _before_f_mapping: [u8; 16],
    pub f_mapping: *mut c_void,
    pub private_data: *mut c_void,
    pub f_inode: *mut c_void,
    pub f_flags: c_uint,
    pub _tail: [u8; 132],
}
const _: [(); 176] = [(); core::mem::size_of::<File>()];
const _: [(); 16] = [(); core::mem::offset_of!(File, f_mapping)];
const _: [(); 24] = [(); core::mem::offset_of!(File, private_data)];
const _: [(); 40] = [(); core::mem::offset_of!(File, f_flags)];

/// `struct ww_acquire_ctx` storage for the selected x86_64 config:
/// RT/debug WW/lockdep disabled.
#[repr(C, align(8))]
pub struct WwAcquireCtx {
    pub task: *mut c_void,
    pub stamp: u64,
    pub acquired: u32,
    pub wounded: u16,
    pub is_wait_die: u16,
}
