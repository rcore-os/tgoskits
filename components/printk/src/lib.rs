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
    ptr::NonNull,
    sync::atomic::{AtomicU64, AtomicUsize},
};

mod dev_printk;
use dev_printk::DevInfo;

/// Maximum stored message length.
pub const MSG_MAX: usize = 1024;

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

/// Special data block logical position values (for fields of
/// `prb_desc.text_blk_lpos`).
///
/// - Bit0 is used to identify if the record has no data block. (Implemented in
///   the `LPOS_DATALESS()` macro.)
///
/// - Bit1 specifies the reason for not having a data block.
///
/// These special values could never be real lpos values because of the
/// meta data and alignment padding of data blocks. (See `to_blk_size()` for
/// details.)
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

/// A descriptor: the complete meta-data for a record.
///
/// `state_var`: A bitwise combination of descriptor ID and descriptor state.
pub struct Desc {
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

/// Meta information about each stored message.
///
/// All fields are set by the printk code except for `seq`, which is
/// set by the ringbuffer code.
#[derive(Clone, Copy)]
pub struct Info {
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

/// A ringbuffer of "struct prb_desc" elements.
struct DescRing {
    count_bits: u32,
    descs: NonNull<Desc>,
    infos: NonNull<UnsafeCell<Info>>,
    head_id: AtomicUsize,
    tail_id: AtomicUsize,
    last_finalized_seq: AtomicU64,
}

impl DescRing {
    #[inline]
    fn count(&self) -> usize {
        1 << self.count_bits
    }
    /// Determine the desc array index from an ID or sequence number.
    #[inline]
    fn index(&self, n: usize) -> usize {
        n & (self.count() - 1)
    }
    /// Get the ID for the same index of the previous wrap as the given ID.
    #[inline]
    fn id_prev_wrap(&self, id: usize) -> usize {
        desc_id(id.wrapping_sub(self.count()))
    }
    /// Returns the descriptor associated with `n`.
    ///
    /// `n` can be either a descriptor ID or a sequence number.
    #[inline]
    fn desc(&self, id: usize) -> &Desc {
        unsafe { &*self.descs.as_ptr().add(self.index(id)) }
    }
    /// Returns the printk_info associated with `n`.
    ///
    /// `n` can be either a descriptor ID or a sequence number.
    #[inline]
    fn info_ptr(&self, id: usize) -> *mut Info {
        unsafe { UnsafeCell::raw_get(self.infos.as_ptr().add(self.index(id))) }
    }
}

/// A ringbuffer of "ID + data" elements.
struct DataRing {
    size_bits: u32,
    data: NonNull<UnsafeCell<u8>>,
    head_lpos: AtomicUsize,
    tail_lpos: AtomicUsize,
}

impl DataRing {
    #[inline]
    fn size(&self) -> usize {
        1 << self.size_bits
    }
    /// Determine the data array index from a logical position.
    #[inline]
    fn index(&self, lpos: usize) -> usize {
        lpos & (self.size() - 1)
    }
    /// Determine how many times the data array has wrapped.
    #[inline]
    fn wraps(&self, lpos: usize) -> usize {
        lpos >> self.size_bits
    }
    /// Get the logical position at index 0 of the current wrap.
    #[inline]
    fn this_wrap_start(&self, lpos: usize) -> usize {
        lpos & !(self.size() - 1)
    }
    /// Raw byte pointer to the data array (contiguous, `size()` bytes).
    #[inline]
    fn base(&self) -> *mut u8 {
        UnsafeCell::raw_get(self.data.as_ptr())
    }
}

pub struct PrintkRingBuffer {
    desc_ring: DescRing,
    data_ring: DataRing,
    fail: AtomicU64,
}

// SAFETY: the ring holds `NonNull` pointers to its backing arrays (established
// by `from_buffers`, valid and unaliased for the ring's lifetime); every access
// to those arrays is synchronized by the descriptor state machine + fences, and
// all other fields are atomics.
unsafe impl Sync for PrintkRingBuffer {}

impl PrintkRingBuffer {
    /// Initialize a ring over caller-provided backing buffers (mirrors Linux
    /// `prb_init`). The descriptor and info arrays are (re)initialized in place;
    /// the text data buffer is left untouched.
    ///
    /// This is the single place the backing-array invariant relied upon by the
    /// `desc`/`info_ptr`/`base` accessors is established, so those need no
    /// further checks.
    ///
    /// # Safety
    /// - `descs` and `infos` must each point to `1 << desc_count_bits` writable,
    ///   properly-aligned elements; `data` to `1 << data_size_bits` bytes.
    /// - All three buffers must stay valid, and must not be aliased by any other
    ///   `PrintkRingBuffer`, for as long as the returned ring is used.
    pub unsafe fn from_buffers(
        descs: NonNull<Desc>,
        infos: NonNull<UnsafeCell<Info>>,
        desc_count_bits: u32,
        data: NonNull<UnsafeCell<u8>>,
        data_size_bits: u32,
    ) -> Self {
        let count = 1usize << desc_count_bits;
        // Initial head/tail id sits at the last array index and overflows into
        // index 0 on the first wrap.
        let head_id = desc_id(((1usize << desc_count_bits) + 1).wrapping_neg());
        // Initial head/tail lpos sits at index 0 and overflows on the first wrap.
        let head_lpos = (1usize << data_size_bits).wrapping_neg();

        for i in 0..count {
            // The last slot is the initial head and tail: a data-less, reusable
            // descriptor.
            let bootstrap = i == count - 1;
            let desc = Desc {
                state_var: AtomicUsize::new(if bootstrap {
                    desc_sv(head_id, DESC_REUSABLE)
                } else {
                    0
                }),
                begin: AtomicUsize::new(if bootstrap { FAILED_LPOS } else { 0 }),
                next: AtomicUsize::new(if bootstrap { FAILED_LPOS } else { 0 }),
            };

            // Bootstrap sequence numbers: slot 0 is the first record a writer
            // reserves (incremented to 0 on that reservation); the last slot
            // reports seq 0 during the bootstrap phase.
            let mut info = Info::new();
            if i == 0 {
                info.seq = (count as u64).wrapping_neg();
            }
            if bootstrap {
                info.seq = 0;
            }

            // SAFETY: the caller guarantees `count` writable, aligned slots in
            // both arrays; this runs before any concurrent access to the ring.
            unsafe {
                descs.as_ptr().add(i).write(desc);
                infos.as_ptr().add(i).write(UnsafeCell::new(info));
            }
        }

        Self {
            desc_ring: DescRing {
                count_bits: desc_count_bits,
                descs,
                infos,
                head_id: AtomicUsize::new(head_id),
                tail_id: AtomicUsize::new(head_id),
                last_finalized_seq: AtomicU64::new(0),
            },
            data_ring: DataRing {
                size_bits: data_size_bits,
                data,
                head_lpos: AtomicUsize::new(head_lpos),
                tail_lpos: AtomicUsize::new(head_lpos),
            },
            fail: AtomicU64::new(0),
        }
    }

    /// Const-construct a ring over already-bootstrapped `'static` backing arrays
    /// (mirrors Linux `DEFINE_PRINTKRB`). Unlike `from_buffers`, the arrays are
    /// *not* touched here — `bootstrap_descs`/`bootstrap_infos` must have set
    /// their initial contents at compile time. Used by `define_printkrb!`.
    ///
    /// # Safety
    /// - `descs`/`infos` must point to `1 << desc_count_bits` elements
    ///   bootstrapped by `bootstrap_descs`/`bootstrap_infos`; `data` to
    ///   `1 << data_size_bits` writable bytes.
    /// - All three must be valid for `'static` and unaliased by any other ring.
    pub const unsafe fn from_static(
        descs: NonNull<Desc>,
        infos: NonNull<UnsafeCell<Info>>,
        desc_count_bits: u32,
        data: NonNull<UnsafeCell<u8>>,
        data_size_bits: u32,
    ) -> Self {
        // Initial head/tail id sits at the last array index; initial head/tail
        // lpos at index 0. Both overflow on the first wrap.
        let head_id = desc_id(((1usize << desc_count_bits) + 1).wrapping_neg());
        let head_lpos = (1usize << data_size_bits).wrapping_neg();

        Self {
            desc_ring: DescRing {
                count_bits: desc_count_bits,
                descs,
                infos,
                head_id: AtomicUsize::new(head_id),
                tail_id: AtomicUsize::new(head_id),
                last_finalized_seq: AtomicU64::new(0),
            },
            data_ring: DataRing {
                size_bits: data_size_bits,
                data,
                head_lpos: AtomicUsize::new(head_lpos),
                tail_lpos: AtomicUsize::new(head_lpos),
            },
            fail: AtomicU64::new(0),
        }
    }
}

/// Build a compile-time-bootstrapped descriptor array for `define_printkrb!`.
/// The last slot is the initial head/tail: a data-less, `reusable` descriptor.
#[doc(hidden)]
pub const fn bootstrap_descs<const N: usize>() -> [Desc; N] {
    let mut arr = [const { Desc::new() }; N];
    let head_id = desc_id(N.wrapping_add(1).wrapping_neg());
    arr[N - 1] = Desc {
        state_var: AtomicUsize::new(desc_sv(head_id, DESC_REUSABLE)),
        begin: AtomicUsize::new(FAILED_LPOS),
        next: AtomicUsize::new(FAILED_LPOS),
    };
    arr
}

/// Build a compile-time-bootstrapped info array for `define_printkrb!`. Slot 0
/// is the first record a writer reserves; the last slot reports seq 0.
#[doc(hidden)]
pub const fn bootstrap_infos<const N: usize>() -> [UnsafeCell<Info>; N] {
    let mut arr = [const { UnsafeCell::new(Info::new()) }; N];
    let mut first = Info::new();
    first.seq = (N as u64).wrapping_neg();
    arr[0] = UnsafeCell::new(first);
    let mut last = Info::new();
    last.seq = 0;
    arr[N - 1] = UnsafeCell::new(last);
    arr
}

/// `Sync` wrapper for `define_printkrb!`'s `UnsafeCell` backing arrays, which
/// are otherwise `!Sync` and cannot back a `static`.
#[doc(hidden)]
#[repr(transparent)]
pub struct SyncWrap<T>(pub T);

// SAFETY: the wrapped array is shared across CPUs only through the ring, whose
// access to it is synchronized by the descriptor state machine + fences — the
// same contract as `unsafe impl Sync for PrintkRingBuffer`.
unsafe impl<T> Sync for SyncWrap<T> {}

/// Define a `static` printk ring buffer with compile-time backing storage
/// (mirrors Linux `DEFINE_PRINTKRB`).
///
/// `desc_bits` sets the number of descriptor slots (`1 << desc_bits`); the text
/// data buffer is `1 << (desc_bits + avg_text_bits)` bytes.
#[macro_export]
macro_rules! define_printkrb {
    ($vis:vis $name:ident, $desc_bits:expr, $avg_text_bits:expr $(,)?) => {
        $vis static $name: $crate::PrintkRingBuffer = {
            const DESC_BITS: u32 = $desc_bits;
            const SIZE_BITS: u32 = $desc_bits + $avg_text_bits;
            const COUNT: usize = 1usize << DESC_BITS;
            const DATA_SIZE: usize = 1usize << SIZE_BITS;

            // `Desc` is `Sync` (atomics); the `UnsafeCell` arrays need `SyncWrap`.
            static DESCS: [$crate::Desc; COUNT] = $crate::bootstrap_descs::<COUNT>();
            static INFOS: $crate::SyncWrap<[::core::cell::UnsafeCell<$crate::Info>; COUNT]> =
                $crate::SyncWrap($crate::bootstrap_infos::<COUNT>());
            static DATA: $crate::SyncWrap<[::core::cell::UnsafeCell<u8>; DATA_SIZE]> =
                $crate::SyncWrap([const { ::core::cell::UnsafeCell::new(0u8) }; DATA_SIZE]);

            // SAFETY: the three sibling statics live for `'static`, are writable
            // (interior mutability via atomics / `UnsafeCell`), bootstrapped at
            // compile time, and used by no other ring.
            unsafe {
                $crate::PrintkRingBuffer::from_static(
                    ::core::ptr::NonNull::new_unchecked(
                        ::core::ptr::addr_of!(DESCS) as *mut $crate::Desc,
                    ),
                    ::core::ptr::NonNull::new_unchecked(
                        ::core::ptr::addr_of!(INFOS.0)
                            as *mut ::core::cell::UnsafeCell<$crate::Info>,
                    ),
                    DESC_BITS,
                    ::core::ptr::NonNull::new_unchecked(
                        ::core::ptr::addr_of!(DATA.0) as *mut ::core::cell::UnsafeCell<u8>,
                    ),
                    SIZE_BITS,
                )
            }
        };
    };
}
