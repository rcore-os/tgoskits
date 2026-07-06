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
//! stress before it is wired into the crate.
#![allow(dead_code)]

use core::{
    cell::UnsafeCell,
    sync::atomic::{
        AtomicU64, AtomicUsize, fence,
        Ordering::{Acquire, Relaxed, Release, SeqCst},
    },
};

// ---- compile-time geometry --------------------------------------------------

/// `count_bits` for the descriptor ring: `DESCS_COUNT = 1 << DESC_COUNT_BITS`.
const DESC_COUNT_BITS: u32 = 11;
const DESCS_COUNT: usize = 1 << DESC_COUNT_BITS;
const DESCS_COUNT_MASK: usize = DESCS_COUNT - 1;

/// `size_bits` for the text data ring: `DATA_SIZE = 1 << DATA_SIZE_BITS`.
const DATA_SIZE_BITS: u32 = 17; // 128 KiB
pub(crate) const DATA_SIZE: usize = 1 << DATA_SIZE_BITS;
const DATA_SIZE_MASK: usize = DATA_SIZE - 1;

/// Maximum stored message length.
pub const MSG_MAX: usize = 1024;

// `state_var` packs the descriptor id (low bits) and a 2-bit state (top bits).
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

// Special data-less lpos sentinels (`LPOS_DATALESS` = low bit set).
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
/// `DESC_ID_PREV_WRAP`: the id of the same slot one wrap earlier.
#[inline]
fn desc_id_prev_wrap(id: usize) -> usize {
    desc_id(id.wrapping_sub(DESCS_COUNT))
}
#[inline]
fn desc_index(n: usize) -> usize {
    n & DESCS_COUNT_MASK
}
#[inline]
fn data_index(lpos: usize) -> usize {
    lpos & DATA_SIZE_MASK
}
/// How many times the data array has wrapped at `lpos`.
#[inline]
fn data_wraps(lpos: usize) -> usize {
    lpos >> DATA_SIZE_BITS
}
/// Start-of-wrap lpos containing `lpos`.
#[inline]
fn data_this_wrap_start(lpos: usize) -> usize {
    lpos & !DATA_SIZE_MASK
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
    // Structured device metadata (SUBSYSTEM=/DEVICE= fields) is omitted for v1.
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
        }
    }
}

/// The text data ring. `data` is plain memory whose accesses are ordered by the
/// descriptor state machine and the surrounding fences (zero-copy writer path).
struct DataRing {
    data: UnsafeCell<[u8; DATA_SIZE]>,
    head_lpos: AtomicUsize,
    tail_lpos: AtomicUsize,
}

struct DescRing {
    descs: [Desc; DESCS_COUNT],
    infos: [UnsafeCell<Info>; DESCS_COUNT],
    head_id: AtomicUsize,
    tail_id: AtomicUsize,
    last_finalized_seq: AtomicU64,
}

struct Prb {
    desc_ring: DescRing,
    data_ring: DataRing,
    fail: AtomicU64,
}

// SAFETY: every field is either an atomic, the `data` buffer, or the `infos`
// array; the raw accesses to the latter two are synchronized by the descriptor
// state machine + fences.
unsafe impl Sync for Prb {}

/// Bootstrap: `head_id`/`tail_id` start at `DESC0_ID` and `descs[0]` starts
/// finalized→reusable so the first real reservation (id 1) recycles slot 0
/// cleanly. `DESC0_ID = desc_id(-(COUNT+1))`.
const DESC0_ID: usize = desc_id(0usize.wrapping_sub(DESCS_COUNT + 1));
const DESC0_SV: usize = desc_sv(DESC0_ID, DESC_REUSABLE);
/// `BLK0_LPOS = -DATA_SIZE`.
const BLK0_LPOS: usize = 0usize.wrapping_sub(DATA_SIZE);

static PRB: Prb = Prb {
    desc_ring: DescRing {
        descs: {
            // Bootstrap: the last descriptor is the seed, set to DESC0_SV with
            // a data-less (FAILED_LPOS) block; the rest are 0.
            let mut a = [const { Desc::new() }; DESCS_COUNT];
            a[DESCS_COUNT - 1].state_var = AtomicUsize::new(DESC0_SV);
            a[DESCS_COUNT - 1].begin = AtomicUsize::new(FAILED_LPOS);
            a[DESCS_COUNT - 1].next = AtomicUsize::new(FAILED_LPOS);
            a
        },
        infos: {
            // Bootstrap: infos[0].seq = -COUNT; infos[COUNT-1].seq = 0 (the
            // latter is the default).
            let mut a = [const { UnsafeCell::new(Info::new()) }; DESCS_COUNT];
            a[0] = UnsafeCell::new(Info {
                seq: 0u64.wrapping_sub(DESCS_COUNT as u64),
                ts_nsec: 0,
                text_len: 0,
                facility: 0,
                flags: 0,
                level: 0,
                caller_id: 0,
            });
            a
        },
        head_id: AtomicUsize::new(DESC0_ID),
        tail_id: AtomicUsize::new(DESC0_ID),
        last_finalized_seq: AtomicU64::new(0),
    },
    data_ring: DataRing {
        data: UnsafeCell::new([0; DATA_SIZE]),
        head_lpos: AtomicUsize::new(BLK0_LPOS),
        tail_lpos: AtomicUsize::new(BLK0_LPOS),
    },
    fail: AtomicU64::new(0),
};

#[inline]
fn desc(id: usize) -> &'static Desc {
    &PRB.desc_ring.descs[desc_index(id)]
}
#[inline]
fn info_ptr(id: usize) -> *mut Info {
    PRB.desc_ring.infos[desc_index(id)].get()
}

/// Volatile load of an `Info` field, ordered by the surrounding state-machine
/// fences (the field may be raced by a descriptor being reused, and is
/// validated by the caller's `state_var` re-read).
#[inline]
fn info_read<T: Copy>(field: *const T) -> T {
    // SAFETY: `field` points to a live, in-bounds field of an `Info` in `PRB`.
    unsafe { field.read_volatile() }
}

/// Volatile store of an `Info` field. The caller owns the `reserved`
/// descriptor, so the write is writer-exclusive.
#[inline]
fn info_write<T: Copy>(field: *mut T, val: T) {
    // SAFETY: `field` points to a live, in-bounds field of an `Info` in `PRB`,
    // and the caller holds the reserved descriptor exclusively.
    unsafe { field.write_volatile(val) }
}

/// Raw pointer to the data block header at `begin_lpos`.
#[inline]
fn block_ptr(begin_lpos: usize) -> *mut u8 {
    // SAFETY: index is masked into the data array bounds.
    unsafe { (PRB.data_ring.data.get() as *mut u8).add(data_index(begin_lpos)) }
}

/// Read/write the block's `id` header (first `usize` of a block). Unaligned
/// access is used because the data buffer has byte alignment.
#[inline]
fn block_read_id(begin_lpos: usize) -> usize {
    // SAFETY: `block_ptr` is in bounds for at least `size_of::<usize>()` bytes.
    unsafe { (block_ptr(begin_lpos) as *const usize).read_unaligned() }
}
#[inline]
fn block_write_id(begin_lpos: usize, id: usize) {
    // SAFETY: as above; caller holds exclusive access to this block.
    unsafe { (block_ptr(begin_lpos) as *mut usize).write_unaligned(id) }
}

// ---- descriptor state machine ----------------------------------------------

/// `get_desc_state`.
fn get_desc_state(id: usize, state_val: usize) -> DescState {
    if id != desc_id(state_val) {
        return DescState::Miss;
    }
    match desc_state_bits(state_val) {
        DESC_RESERVED => DescState::Reserved,
        DESC_COMMITTED => DescState::Committed,
        DESC_FINALIZED => DescState::Finalized,
        _ => DescState::Reusable,
    }
}

/// A reader's consistent copy of a descriptor (`desc_read` output).
struct DescCopy {
    state_var: usize,
    begin: usize,
    next: usize,
    seq: u64,
    caller_id: u64,
}

/// `desc_read`: read a descriptor and its queried state. `begin`/`next`/`seq`/
/// `caller_id` are only valid if the returned state is consistent.
fn desc_read(id: usize) -> (DescState, DescCopy) {
    let d = desc(id);
    let nfo = info_ptr(id);

    // desc_read:A
    let mut state_val = d.state_var.load(Relaxed);
    let mut d_state = get_desc_state(id, state_val);
    if d_state == DescState::Miss || d_state == DescState::Reserved {
        return (
            d_state,
            DescCopy {
                state_var: state_val,
                begin: 0,
                next: 0,
                seq: 0,
                caller_id: 0,
            },
        );
    }

    // desc_read:B — load state before copying content.
    fence(Acquire);

    // desc_read:C — copy the descriptor content. Only `seq` and `caller_id` of
    // the info are needed during traversal.
    let begin = d.begin.load(Relaxed);
    let next = d.next.load(Relaxed);
    let seq = info_read(core::ptr::addr_of!((*nfo).seq));
    let caller_id = info_read(core::ptr::addr_of!((*nfo).caller_id)) as u64;

    // desc_read:D — load content before re-checking state.
    fence(Acquire);

    // desc_read:E — re-read the state, which may have changed.
    state_val = d.state_var.load(Relaxed);
    d_state = get_desc_state(id, state_val);

    (
        d_state,
        DescCopy {
            state_var: state_val,
            begin,
            next,
            seq,
            caller_id,
        },
    )
}

/// `desc_make_reusable`: finalized → reusable (relaxed CAS).
fn desc_make_reusable(id: usize) {
    let _ = desc(id).state_var.compare_exchange(
        desc_sv(id, DESC_FINALIZED),
        desc_sv(id, DESC_REUSABLE),
        Relaxed,
        Relaxed,
    );
}

// ---- data ring --------------------------------------------------------------

/// `data_make_reusable`: retire descriptors of blocks in `[lpos_begin, lpos_end)`.
/// Returns `Some(next_begin)` on success, `None` on a still-live/recycled block.
fn data_make_reusable(mut lpos_begin: usize, lpos_end: usize) -> Option<usize> {
    while (lpos_end.wrapping_sub(lpos_begin)).wrapping_sub(1) < DATA_SIZE {
        // data_make_reusable:A — racy read of the block id.
        let id = block_read_id(lpos_begin);

        // data_make_reusable:B
        let (d_state, copy) = desc_read(id);
        match d_state {
            DescState::Miss | DescState::Reserved | DescState::Committed => return None,
            DescState::Finalized => {
                if copy.begin != lpos_begin {
                    return None;
                }
                desc_make_reusable(id);
            }
            DescState::Reusable => {
                if copy.begin != lpos_begin {
                    return None;
                }
            }
        }
        lpos_begin = copy.next;
    }
    Some(lpos_begin)
}

/// `data_push_tail`: advance the data tail to at least `lpos`.
fn data_push_tail(lpos: usize) -> bool {
    if lpos_dataless(lpos) {
        return true;
    }

    // data_push_tail:A
    let mut tail_lpos = PRB.data_ring.tail_lpos.load(Relaxed);

    while (lpos.wrapping_sub(tail_lpos)).wrapping_sub(1) < DATA_SIZE {
        match data_make_reusable(tail_lpos, lpos) {
            None => {
                // data_push_tail:B
                fence(Acquire);
                // data_push_tail:C
                let tail_lpos_new = PRB.data_ring.tail_lpos.load(Relaxed);
                if tail_lpos_new == tail_lpos {
                    return false;
                }
                tail_lpos = tail_lpos_new;
                continue;
            }
            Some(next_lpos) => {
                // data_push_tail:D — full-barrier CAS.
                match PRB.data_ring.tail_lpos.compare_exchange(
                    tail_lpos,
                    next_lpos,
                    SeqCst,
                    Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => {
                        tail_lpos = actual;
                    }
                }
            }
        }
    }
    true
}

/// `desc_push_tail`: retire descriptor `tail_id` and advance the desc tail.
fn desc_push_tail(tail_id: usize) -> bool {
    let (d_state, copy) = desc_read(tail_id);

    match d_state {
        DescState::Miss => {
            // One wrap behind expected → still being reserved; treat as reserved.
            if desc_id(copy.state_var) == desc_id_prev_wrap(tail_id) {
                return false;
            }
            // The id changed: another writer already recycled it. Success.
            return true;
        }
        DescState::Reserved | DescState::Committed => return false,
        DescState::Finalized => desc_make_reusable(tail_id),
        DescState::Reusable => {}
    }

    // Data blocks must be invalidated before the descriptor can be recycled.
    if !data_push_tail(copy.next) {
        return false;
    }

    // The next descriptor must be finalized/reusable before pushing the tail.
    let (next_state, _) = desc_read(desc_id(tail_id + 1)); // desc_push_tail:A
    if next_state == DescState::Finalized || next_state == DescState::Reusable {
        // desc_push_tail:B — full-barrier CAS.
        let _ = PRB.desc_ring.tail_id.compare_exchange(
            tail_id,
            desc_id(tail_id + 1),
            SeqCst,
            Relaxed,
        );
    } else {
        // desc_push_tail:C
        fence(Acquire);
        // desc_push_tail:D
        if PRB.desc_ring.tail_id.load(Relaxed) == tail_id {
            return false;
        }
    }
    true
}

/// `desc_reserve`: reserve a new descriptor id, retiring the oldest if needed.
fn desc_reserve() -> Option<usize> {
    // desc_reserve:A
    let mut head_id = PRB.desc_ring.head_id.load(Relaxed);
    let id;
    let id_prev_wrap;

    loop {
        let new_id = desc_id(head_id + 1);
        let prev_wrap = desc_id_prev_wrap(new_id);

        // desc_reserve:B — head read before tail read.
        fence(Acquire);

        // desc_reserve:C
        if prev_wrap == PRB.desc_ring.tail_id.load(Relaxed) && !desc_push_tail(prev_wrap) {
            return None;
        }

        // desc_reserve:D — full-barrier CAS publishing the new head id.
        match PRB
            .desc_ring
            .head_id
            .compare_exchange(head_id, new_id, SeqCst, Relaxed)
        {
            Ok(_) => {
                id = new_id;
                id_prev_wrap = prev_wrap;
                break;
            }
            Err(actual) => head_id = actual,
        }
    }

    let d = desc(id);

    // desc_reserve:E — verify the recycled descriptor's old state (ABA).
    let prev_state_val = d.state_var.load(Relaxed);
    if prev_state_val != 0
        && get_desc_state(id_prev_wrap, prev_state_val) != DescState::Reusable
    {
        // WARN_ON_ONCE: inconsistent recycle.
        return None;
    }

    // desc_reserve:F — claim the slot: prev → (id, reserved).
    if d
        .state_var
        .compare_exchange(prev_state_val, desc_sv(id, DESC_RESERVED), SeqCst, Relaxed)
        .is_err()
    {
        return None;
    }

    // desc_reserve:G — data in the descriptor may now be modified.
    Some(id)
}

/// `get_next_lpos`: end position of a block of `size` starting at `lpos`.
fn get_next_lpos(lpos: usize, size: usize) -> usize {
    let next_lpos = lpos + size;
    if data_wraps(lpos) == data_wraps(next_lpos) {
        return next_lpos;
    }
    // Wrapping blocks store their data at the beginning of the next wrap.
    data_this_wrap_start(next_lpos) + size
}

/// `data_alloc`: allocate a data block of `size` text bytes for `id`. Returns
/// `(begin, next, data_ptr)` or sets a data-less block and returns `None`.
fn data_alloc(size: usize, id: usize) -> Option<(usize, usize, *mut u8)> {
    if size == 0 {
        return None; // caller records EMPTY_LINE_LPOS
    }
    let size = to_blk_size(size);

    let mut begin_lpos = PRB.data_ring.head_lpos.load(Relaxed);
    let next_lpos;
    loop {
        let candidate = get_next_lpos(begin_lpos, size);
        if !data_push_tail(candidate.wrapping_sub(DATA_SIZE)) {
            return None; // FAILED_LPOS
        }
        // data_alloc:A — full-barrier CAS publishing the new head lpos.
        match PRB.data_ring.head_lpos.compare_exchange(
            begin_lpos,
            candidate,
            SeqCst,
            Relaxed,
        ) {
            Ok(_) => {
                next_lpos = candidate;
                break;
            }
            Err(actual) => begin_lpos = actual,
        }
    }

    // data_alloc:B — write the block id.
    block_write_id(begin_lpos, id);

    let data_begin = if data_wraps(begin_lpos) != data_wraps(next_lpos) {
        // Wrapping block: data lives at offset 0 of the next wrap.
        block_write_id(0, id);
        block_ptr(0)
    } else {
        block_ptr(begin_lpos)
    };
    let id_sz = core::mem::size_of::<usize>();
    // SAFETY: exclusive access to the reserved block; data follows the id header.
    let data_ptr = unsafe { data_begin.add(id_sz) };

    Some((begin_lpos, next_lpos, data_ptr))
}

/// `space_used`: bytes the block occupies (including wrap padding).
fn space_used(begin: usize, next: usize) -> usize {
    if blk_dataless(begin, next) {
        return 0;
    }
    if data_wraps(begin) == data_wraps(next) {
        data_index(next) - data_index(begin)
    } else {
        data_index(next) + DATA_SIZE - data_index(begin)
    }
}

/// `get_data`: validate `begin`/`next` and return `(data_ptr, data_size)` of the
/// writer data (header excluded), or `None` if not legal/available.
fn get_data(begin: usize, next: usize) -> Option<(*const u8, usize)> {
    if blk_dataless(begin, next) {
        if begin == EMPTY_LINE_LPOS && next == EMPTY_LINE_LPOS {
            return Some((b"".as_ptr(), 0));
        }
        return None;
    }

    let id_sz = core::mem::size_of::<usize>();
    let (blk_begin, mut data_size);
    if data_wraps(begin) == data_wraps(next) && begin < next {
        blk_begin = begin;
        data_size = next - begin;
    } else if data_wraps(begin + DATA_SIZE) == data_wraps(next) {
        blk_begin = 0;
        data_size = data_index(next);
    } else {
        return None; // WARN_ON_ONCE: illegal block
    }

    if begin % id_sz != 0 || next % id_sz != 0 || data_size < id_sz {
        return None;
    }
    data_size -= id_sz;
    // SAFETY: validated, aligned, in-bounds block; data follows the id header.
    let data_ptr = unsafe { block_ptr(blk_begin).add(id_sz) as *const u8 };
    Some((data_ptr, data_size))
}

// ---- reserve / commit -------------------------------------------------------

const EINVAL: i32 = -22;
const ENOENT: i32 = -2;

/// A successfully reserved record (`prb_reserved_entry`; `rb` is the global PRB
/// and IRQ flags are omitted — see the note on the IRQ-disable deviation).
pub struct ReservedEntry {
    id: usize,
    text_space: usize,
}

/// `data_check_size`.
fn data_check_size(size: usize) -> bool {
    if size == 0 {
        return true;
    }
    to_blk_size(size) <= DATA_SIZE - core::mem::size_of::<usize>()
}

/// `desc_make_final`: committed → finalized (relaxed CAS), then update the
/// last-finalized sequence on success.
fn desc_make_final(id: usize) {
    if desc(id)
        .state_var
        .compare_exchange(
            desc_sv(id, DESC_COMMITTED),
            desc_sv(id, DESC_FINALIZED),
            Relaxed,
            Relaxed,
        )
        .is_ok()
    {
        desc_update_last_finalized();
    }
}

/// `desc_last_finalized_seq` (ulseq == u64seq on 64-bit).
fn desc_last_finalized_seq() -> u64 {
    PRB.desc_ring.last_finalized_seq.load(Acquire) // desc_last_finalized_seq:A
}

/// `desc_update_last_finalized`.
fn desc_update_last_finalized() {
    let mut old_seq = desc_last_finalized_seq();
    loop {
        let mut finalized_seq = old_seq;
        let mut try_seq = finalized_seq + 1;
        while _prb_read_valid(&mut try_seq, None) {
            finalized_seq = try_seq;
            try_seq += 1;
        }
        if finalized_seq == old_seq {
            return;
        }
        // desc_update_last_finalized:A — release CAS.
        match PRB.desc_ring.last_finalized_seq.compare_exchange(
            old_seq,
            finalized_seq,
            Release,
            Relaxed,
        ) {
            Ok(_) => return,
            Err(actual) => old_seq = actual, // try_again
        }
    }
}

/// `prb_reserve`: reserve space for a record of `text_buf_size` text bytes.
/// Returns the entry, a pointer to the (zero-copy) text buffer, and its size.
pub fn prb_reserve(text_buf_size: usize) -> Option<(ReservedEntry, *mut u8, usize)> {
    if !data_check_size(text_buf_size) {
        return None;
    }

    let id = match desc_reserve() {
        Some(id) => id,
        None => {
            PRB.fail.fetch_add(1, Relaxed);
            return None;
        }
    };

    let nfo = info_ptr(id);
    let seq = info_read(core::ptr::addr_of!((*nfo).seq));
    // Zero the info, preserving the saved `seq` for the computation below.
    info_write(nfo, Info::new());

    // Bootstrap-aware sequence assignment.
    let new_seq = if seq == 0 && desc_index(id) != 0 {
        desc_index(id) as u64
    } else {
        seq + DESCS_COUNT as u64
    };
    info_write(core::ptr::addr_of_mut!((*nfo).seq), new_seq);

    // Finalize the previous descriptor (if any) so it becomes readable.
    if new_seq > 0 {
        desc_make_final(desc_id(id.wrapping_sub(1)));
    }

    let d = desc(id);
    if text_buf_size == 0 {
        d.begin.store(EMPTY_LINE_LPOS, Relaxed);
        d.next.store(EMPTY_LINE_LPOS, Relaxed);
        let text_space = space_used(EMPTY_LINE_LPOS, EMPTY_LINE_LPOS);
        return Some((ReservedEntry { id, text_space }, core::ptr::null_mut(), 0));
    }

    match data_alloc(text_buf_size, id) {
        Some((begin, next, ptr)) => {
            d.begin.store(begin, Relaxed);
            d.next.store(next, Relaxed);
            let text_space = space_used(begin, next);
            Some((ReservedEntry { id, text_space }, ptr, text_buf_size))
        }
        None => {
            // Data allocation failed: commit a data-less record and report fail.
            d.begin.store(FAILED_LPOS, Relaxed);
            d.next.store(FAILED_LPOS, Relaxed);
            prb_commit(&ReservedEntry { id, text_space: 0 });
            None
        }
    }
}

/// Set the writer-owned metadata of a reserved record before committing.
///
/// `priority` is the syslog priority (`facility << 3 | level`); it is split
/// into the stored `facility`/`level` fields, matching how a `<pri>` prefix is
/// decoded at the `/dev/kmsg` boundary.
pub fn set_info(e: &ReservedEntry, ts_nsec: u64, priority: u8, flags: u8, text_len: usize) {
    let nfo = info_ptr(e.id);
    info_write(core::ptr::addr_of_mut!((*nfo).ts_nsec), ts_nsec);
    info_write(core::ptr::addr_of_mut!((*nfo).text_len), text_len as u16);
    info_write(core::ptr::addr_of_mut!((*nfo).facility), priority >> 3);
    info_write(core::ptr::addr_of_mut!((*nfo).level), priority & 0x7);
    info_write(core::ptr::addr_of_mut!((*nfo).flags), flags);
}

/// Full text space used by a reserved record (`prb_record_text_space`).
pub fn record_text_space(e: &ReservedEntry) -> usize {
    e.text_space
}

/// `_prb_commit`.
fn _prb_commit(e: &ReservedEntry, state_val: usize) {
    let d = desc(e.id);
    // _prb_commit:B — full-barrier CAS (reserved → state_val).
    let _ = d.state_var.compare_exchange(
        desc_sv(e.id, DESC_RESERVED),
        desc_sv(e.id, state_val),
        SeqCst,
        Relaxed,
    );
}

/// `prb_commit`: commit (the record becomes readable only once finalized).
pub fn prb_commit(e: &ReservedEntry) {
    _prb_commit(e, DESC_COMMITTED);
    // prb_commit:A
    if PRB.desc_ring.head_id.load(Relaxed) != e.id {
        desc_make_final(e.id);
    }
}

/// `prb_final_commit`: commit and finalize, making the record readable now.
pub fn prb_final_commit(e: &ReservedEntry) {
    _prb_commit(e, DESC_FINALIZED);
    desc_update_last_finalized();
}

// ---- reader -----------------------------------------------------------------

/// Metadata returned to a reader.
#[derive(Default, Clone, Copy)]
pub struct ReadInfo {
    pub seq: u64,
    pub ts_nsec: u64,
    pub caller_id: u32,
    /// Syslog priority, reconstructed as `facility << 3 | level`.
    pub priority: u8,
    pub flags: u8,
    pub text_len: usize,
}

/// A reader's request/result buffers (`printk_record`).
pub struct Rec<'a> {
    pub info: Option<&'a mut ReadInfo>,
    pub buf: Option<&'a mut [u8]>,
    pub copied: usize,
}

/// `copy_data`: copy up to `len` text bytes of the block into `buf` (if any).
/// Returns the number of bytes copied, or `Err` if the data is unavailable.
fn copy_data(begin: usize, next: usize, len: usize, buf: Option<&mut [u8]>) -> Result<usize, ()> {
    let (data, data_size) = get_data(begin, next).ok_or(())?;
    if data_size < len {
        return Err(());
    }
    if let Some(buf) = buf {
        let n = buf.len().min(len);
        // SAFETY: `data` is a valid pointer to `data_size >= len >= n` bytes in
        // the data ring; `buf` is a distinct caller slice.
        unsafe { core::ptr::copy_nonoverlapping(data, buf.as_mut_ptr(), n) };
        return Ok(n);
    }
    Ok(0)
}

/// `desc_read_finalized_seq`: read a descriptor and verify it is finalized with
/// sequence `seq`. Returns `(0 | -EINVAL | -ENOENT, copy)`.
fn desc_read_finalized_seq(id: usize, seq: u64) -> (i32, DescCopy) {
    let (d_state, copy) = desc_read(id);
    if matches!(
        d_state,
        DescState::Miss | DescState::Reserved | DescState::Committed
    ) || copy.seq != seq
    {
        return (EINVAL, copy);
    }
    if d_state == DescState::Reusable || (copy.begin == FAILED_LPOS && copy.next == FAILED_LPOS) {
        return (ENOENT, copy);
    }
    (0, copy)
}

/// `prb_read`: copy record `seq` into `rec` (if provided). Returns 0/-EINVAL/-ENOENT.
fn prb_read(seq: u64, rec: Option<&mut Rec>) -> i32 {
    let idx = seq as usize;
    let id = desc_id(desc(idx).state_var.load(Relaxed));

    let (err, copy) = desc_read_finalized_seq(id, seq);
    let Some(rec) = rec else { return err };
    if err != 0 {
        return err;
    }

    // Copy the metadata (matches `memcpy(r->info, info, ...)`), field by field
    // to avoid reading the struct's padding bytes.
    let nfo = info_ptr(idx);
    let text_len = info_read(core::ptr::addr_of!((*nfo).text_len)) as usize;
    if let Some(out) = rec.info.as_deref_mut() {
        let facility = info_read(core::ptr::addr_of!((*nfo).facility));
        let level = info_read(core::ptr::addr_of!((*nfo).level));
        out.seq = info_read(core::ptr::addr_of!((*nfo).seq));
        out.ts_nsec = info_read(core::ptr::addr_of!((*nfo).ts_nsec));
        out.caller_id = info_read(core::ptr::addr_of!((*nfo).caller_id));
        out.priority = (facility << 3) | (level & 0x7);
        out.flags = info_read(core::ptr::addr_of!((*nfo).flags));
        out.text_len = text_len;
    }

    match copy_data(copy.begin, copy.next, text_len, rec.buf.as_deref_mut()) {
        Ok(n) => rec.copied = n,
        Err(()) => return ENOENT,
    }

    // Re-verify the record is still finalized with the same seq.
    desc_read_finalized_seq(id, seq).0
}

/// `prb_first_seq`: sequence number of the tail (oldest) descriptor.
fn prb_first_seq() -> u64 {
    loop {
        let id = PRB.desc_ring.tail_id.load(Relaxed); // prb_first_seq:A
        let (d_state, copy) = desc_read(id); // prb_first_seq:B
        if d_state == DescState::Finalized || d_state == DescState::Reusable {
            return copy.seq;
        }
        fence(Acquire); // prb_first_seq:C
    }
}

/// `prb_next_reserve_seq`: sequence number that the next reserved record will get.
fn prb_next_reserve_seq() -> u64 {
    loop {
        let last_finalized_seq = desc_last_finalized_seq();
        let head_id = PRB.desc_ring.head_id.load(Relaxed); // prb_next_reserve_seq:A
        let state = desc(last_finalized_seq as usize).state_var.load(Relaxed);
        let mut last_finalized_id = desc_id(state);

        let (err, _) = desc_read_finalized_seq(last_finalized_id, last_finalized_seq);
        if err == EINVAL {
            if last_finalized_seq == 0 {
                if head_id == DESC0_ID {
                    return 0;
                }
                last_finalized_id = DESC0_ID + 1;
            } else {
                continue; // try_again
            }
        }
        let diff = head_id.wrapping_sub(last_finalized_id);
        return last_finalized_seq + diff as u64 + 1;
    }
}

/// `_prb_read_valid`: read `*seq` or, if gone, advance `*seq` to the next
/// available record. Returns false if no record at/after `*seq` is available.
fn _prb_read_valid(seq: &mut u64, mut rec: Option<&mut Rec>) -> bool {
    loop {
        let err = prb_read(*seq, rec.as_deref_mut());
        if err == 0 {
            return true;
        }
        let tail_seq = prb_first_seq();
        if *seq < tail_seq {
            *seq = tail_seq;
        } else if err == ENOENT {
            *seq += 1;
        } else if axpanic::oops_in_progress() && (*seq + 1) < prb_next_reserve_seq() {
            *seq += 1;
        } else {
            return false;
        }
    }
}

/// `prb_first_valid_seq`: oldest available sequence number (0 if empty).
pub fn first_valid_seq() -> u64 {
    let mut seq = 0;
    if !_prb_read_valid(&mut seq, None) {
        return 0;
    }
    seq
}

/// `prb_next_seq`: sequence number after the last available record.
pub fn next_seq() -> u64 {
    let mut seq = desc_last_finalized_seq();
    if seq != 0 {
        seq += 1;
    }
    while _prb_read_valid(&mut seq, None) {
        seq += 1;
    }
    seq
}

/// Read the first available record at/after `seq` into `out_info` (+ optional
/// text buffer), like `prb_read_valid`. Returns the bytes copied, or `None`.
pub fn read_valid(seq: u64, out_info: &mut ReadInfo, buf: Option<&mut [u8]>) -> Option<usize> {
    let mut s = seq;
    let mut rec = Rec {
        info: Some(out_info),
        buf,
        copied: 0,
    };
    if _prb_read_valid(&mut s, Some(&mut rec)) {
        Some(rec.copied)
    } else {
        None
    }
}
