//! Lockless printk ring buffer.
//!
//! Two rings: a descriptor ring (fixed records: a `state_var` state machine
//! plus metadata) and a text data ring (variable-length, zero-copy, contiguous
//! blocks with trailing dead-space when a block would wrap). Writers reserve via
//! CAS on `head_id`/`head_lpos` and publish through the descriptor state
//! machine; readers stay consistent by re-reading `state_var`.
//!
//! Memory-ordering model — the atomic ordering each operation relies on:
//! - read barrier   → `fence(Acquire)`
//! - write barrier  → `fence(Release)`
//! - full barrier   → `fence(SeqCst)`
//! - plain read     → `load(Relaxed)`
//! - plain store    → `store(Relaxed)`
//! - full CAS       → `compare_exchange(SeqCst, SeqCst)`
//! - relaxed CAS    → `compare_exchange(Relaxed, Relaxed)`
//! - release CAS    → `compare_exchange(Release, Relaxed)`
//! - acquire read   → `load(Acquire)`
//!
//! `unsafe` is used for the zero-copy data buffer and the per-record `Info`;
//! both are plain memory whose visibility is ordered solely by the descriptor
//! state machine plus the surrounding fences. The racy info fields are accessed
//! with `read_volatile`/`write_volatile` and stay consistent only by virtue of
//! the descriptor `state_var` re-read — so this MUST be validated under SMP
//! stress before it is relied upon.
#![no_std]
#![allow(dead_code)]

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU64, AtomicUsize},
};

mod dev_printk;
use dev_printk::DevInfo;

/// Maximum stored message length.
pub const MSG_MAX: usize = 1024;

// ---- size-independent bit layout --------------------------------------------
//
// The descriptor `state_var` packs the id (low bits) and a 2-bit state (top
// bits). This layout depends only on the machine word width, never on the ring
// geometry, so it stays as free constants/functions shared by every instance.

const DESC_SV_BITS: u32 = usize::BITS;
const DESC_FLAGS_SHIFT: u32 = DESC_SV_BITS - 2;
const DESC_FLAGS_MASK: usize = 0b11 << DESC_FLAGS_SHIFT;
const DESC_ID_MASK: usize = !DESC_FLAGS_MASK;

// enum desc_state (desc_miss is represented as `None` from `get_desc_state`).
const DESC_RESERVED: usize = 0x0;
const DESC_COMMITTED: usize = 0x1;
const DESC_FINALIZED: usize = 0x2;
const DESC_REUSABLE: usize = 0x3;

#[derive(PartialEq, Eq, Clone, Copy)]
enum DescState {
    Miss,
    Reserved,
    Committed,
    Finalized,
    Reusable,
}

// Special data-less lpos sentinels (`LPOS_DATALESS` = low bit set). These never
// collide with real positions because blocks are aligned up to `usize` size.
const FAILED_LPOS: usize = 0x1;
const EMPTY_LINE_LPOS: usize = 0x3;

#[inline]
const fn desc_id(sv: usize) -> usize {
    sv & DESC_ID_MASK
}
#[inline]
fn desc_state_bits(sv: usize) -> usize {
    (sv >> DESC_FLAGS_SHIFT) & 0b11
}
#[inline]
const fn desc_sv(id: usize, state: usize) -> usize {
    (state << DESC_FLAGS_SHIFT) | (id & DESC_ID_MASK)
}
#[inline]
fn lpos_dataless(lpos: usize) -> bool {
    (lpos & 1) != 0
}
#[inline]
fn blk_dataless(begin: usize, next: usize) -> bool {
    lpos_dataless(begin) && lpos_dataless(next)
}

/// `to_blk_size`: add the block header (`id`) and align up to `id` size.
#[inline]
fn to_blk_size(size: usize) -> usize {
    let id_sz = core::mem::size_of::<usize>();
    (size + id_sz).next_multiple_of(id_sz)
}

// ---- rings ------------------------------------------------------------------

/// One descriptor: the atomic state machine plus the data-block location.
/// The block location is stored as its `begin`/`next` logical positions.
struct Desc {
    state_var: AtomicUsize,
    begin: AtomicUsize,
    next: AtomicUsize,
}

impl Desc {
    const fn new() -> Self {
        Self {
            state_var: AtomicUsize::new(0),
            begin: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
        }
    }
}

/// Per-record metadata, valid once the descriptor is consistent.
///
/// Plain fields (not atomics): they are writer-exclusive while the descriptor is
/// `reserved` and read-only once finalized; their visibility is published by the
/// `state_var` release/acquire and validated by the reader's `state_var`
/// re-read. Accessed via volatile loads/stores through the enclosing
/// `UnsafeCell`.
#[derive(Clone, Copy)]
struct Info {
    /// Sequence number.
    seq: u64,
    /// Timestamp in nanoseconds.
    ts_nsec: u64,
    /// Length of the text message.
    text_len: u16,
    /// Syslog facility.
    facility: u8,
    /// Internal record flags (5 bits used).
    flags: u8,
    /// Syslog level (3 bits used).
    level: u8,
    /// Thread id or processor id.
    caller_id: u32,
    /// Structured device metadata (`SUBSYSTEM=`/`DEVICE=`).
    dev_info: DevInfo,
}

impl Info {
    const fn new() -> Self {
        Self {
            seq: 0,
            ts_nsec: 0,
            text_len: 0,
            facility: 0,
            flags: 0,
            level: 0,
            caller_id: 0,
            dev_info: DevInfo::new(),
        }
    }
}

/// The descriptor ring. `descs`/`infos` point to backing arrays of
/// `1 << count_bits` elements, provided either statically (see the
/// `define_printkrb!` macro) or at runtime (see [`Prb::from_buffers`]). The
/// geometry (`count_bits`) is carried per instance rather than fixed at compile
/// time, so the same code serves rings of any size.
struct DescRing {
    count_bits: u32,
    descs: *const Desc,
    infos: *const UnsafeCell<Info>,
    head_id: AtomicUsize,
    tail_id: AtomicUsize,
    last_finalized_seq: AtomicU64,
}

impl DescRing {
    /// `DESCS_COUNT`: number of descriptor slots.
    #[inline]
    fn count(&self) -> usize {
        1 << self.count_bits
    }
    /// `DESC_INDEX`: descriptor array index for an id or sequence number.
    #[inline]
    fn index(&self, n: usize) -> usize {
        n & (self.count() - 1)
    }
    /// `DESC_ID_PREV_WRAP`: the id of the same slot one wrap earlier.
    #[inline]
    fn id_prev_wrap(&self, id: usize) -> usize {
        desc_id(id.wrapping_sub(self.count()))
    }
    /// The descriptor for `id` (masked into the backing array).
    #[inline]
    fn desc(&self, id: usize) -> &Desc {
        // SAFETY: `descs` points to `count()` initialized descriptors; `index`
        // masks into bounds.
        unsafe { &*self.descs.add(self.index(id)) }
    }
    /// A raw pointer to the `Info` for `id` (behind its `UnsafeCell`).
    #[inline]
    fn info_ptr(&self, id: usize) -> *mut Info {
        // SAFETY: `infos` points to `count()` initialized cells; `index` masks
        // into bounds.
        unsafe { (*self.infos.add(self.index(id))).get() }
    }
}

/// The text data ring. `data` points to a backing array of `1 << size_bits`
/// plain bytes whose accesses are ordered by the descriptor state machine and
/// the surrounding fences (zero-copy writer path). The geometry (`size_bits`)
/// is carried per instance.
struct DataRing {
    size_bits: u32,
    data: *const UnsafeCell<u8>,
    head_lpos: AtomicUsize,
    tail_lpos: AtomicUsize,
}

impl DataRing {
    /// `DATA_SIZE`: byte capacity of the data ring.
    #[inline]
    fn size(&self) -> usize {
        1 << self.size_bits
    }
    /// `DATA_INDEX`: data array index for a logical position.
    #[inline]
    fn index(&self, lpos: usize) -> usize {
        lpos & (self.size() - 1)
    }
    /// `DATA_WRAPS`: how many times the data array has wrapped at `lpos`.
    #[inline]
    fn wraps(&self, lpos: usize) -> usize {
        lpos >> self.size_bits
    }
    /// `DATA_THIS_WRAP_START_LPOS`: start-of-wrap lpos containing `lpos`.
    #[inline]
    fn this_wrap_start(&self, lpos: usize) -> usize {
        lpos & !(self.size() - 1)
    }
    /// Raw byte pointer to the data array (contiguous, `size()` bytes).
    #[inline]
    fn base(&self) -> *mut u8 {
        self.data as *mut u8
    }
}

pub struct Prb {
    desc_ring: DescRing,
    data_ring: DataRing,
    fail: AtomicU64,
}

// SAFETY: the ring holds raw pointers to its backing arrays (which are either
// `'static` or owned for the ring's lifetime); every access to those arrays is
// synchronized by the descriptor state machine + fences, and all other fields
// are atomics.
unsafe impl Sync for Prb {}
