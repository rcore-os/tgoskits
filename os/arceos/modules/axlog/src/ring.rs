//! Lockless kernel log ring buffer — a two-ring design inspired by Linux's
//! printk ringbuffer (`kernel/printk/printk_ringbuffer.c`):
//!
//! - **descriptor ring**: a fixed array of descriptors, each a small state
//!   machine (`state_var`) plus record metadata (timestamp, priority, text
//!   location). Concurrent writers reserve a descriptor by advancing `head_id`
//!   with a CAS.
//! - **text data ring**: a byte buffer holding the variable-length messages.
//!   Writers reserve a block by advancing `data_head` with a CAS.
//!
//! There are no locks: writers publish a record with a release store on its
//! `state_var`, and readers stay consistent by acquiring that `state_var` and
//! re-reading it after copying. This keeps logging usable in IRQ/NMI/panic
//! context, where a lock could deadlock.
//!
//! This is written from scratch (not a line-by-line port) and is simplified
//! relative to Linux in three ways:
//! - logical positions (`*_id`, `*_lpos`) are monotonic `u64` that never wrap
//!   in practice, avoiding Linux's wrap-aware position arithmetic;
//! - each data block stores its own length inline (`[id:u64][len:u32][text]`),
//!   so the tail can be walked without Linux's dead-space/wrap markers;
//! - records are never continued, so the state machine has three states
//!   (`reserved`/`finalized`/`reusable`) rather than four.
//!
//! It supports concurrent multi-writer use and must be validated under SMP
//! stress before being relied upon.

use core::sync::atomic::{
    AtomicU8, AtomicU64,
    Ordering::{Acquire, Relaxed, Release},
};

/// Number of descriptors (= max retained records). Power of two.
const DESC_COUNT_BITS: u32 = 11;
const DESC_COUNT: u64 = 1 << DESC_COUNT_BITS;
const DESC_INDEX_MASK: u64 = DESC_COUNT - 1;

/// Text data ring size in bytes. Power of two; Linux's default is 128 KiB.
const DATA_SIZE_BITS: u32 = 17;
const DATA_SIZE: usize = 1 << DATA_SIZE_BITS;
const DATA_INDEX_MASK: u64 = (DATA_SIZE as u64) - 1;

/// Maximum stored message length (Linux `LOG_LINE_MAX`-ish). Longer messages
/// are truncated on store.
pub const MSG_MAX: usize = 1024;

/// Inline block header: owner id (`u64`) + text length (`u32`).
const BLOCK_HDR: u64 = 8 + 4;

// `state_var` packs the descriptor id (low 62 bits) and a 2-bit state.
const DESC_FLAGS_SHIFT: u32 = 62;
const DESC_FLAGS_MASK: u64 = 0b11 << DESC_FLAGS_SHIFT;
const DESC_ID_MASK: u64 = !DESC_FLAGS_MASK;

const ST_RESERVED: u64 = 0;
const ST_FINALIZED: u64 = 2;
const ST_REUSABLE: u64 = 3;

fn make_sv(id: u64, state: u64) -> u64 {
    (id & DESC_ID_MASK) | (state << DESC_FLAGS_SHIFT)
}
fn sv_id(sv: u64) -> u64 {
    sv & DESC_ID_MASK
}
fn sv_state(sv: u64) -> u64 {
    (sv & DESC_FLAGS_MASK) >> DESC_FLAGS_SHIFT
}

/// Resolve a slot's state for the expected `id`. `None` means "miss": the slot
/// holds a different generation (it was recycled). An all-zero `state_var` is an
/// empty, never-used slot, treated as reusable.
fn desc_state(id: u64, sv: u64) -> Option<u64> {
    if sv == 0 {
        return Some(ST_REUSABLE);
    }
    if sv_id(sv) != (id & DESC_ID_MASK) {
        return None;
    }
    Some(sv_state(sv))
}

struct Desc {
    /// Packed (id, state).
    state_var: AtomicU64,
    /// Logical position of this record's data block (valid once finalized).
    begin_lpos: AtomicU64,
    /// Monotonic timestamp in microseconds.
    ts_us: AtomicU64,
    /// `priority` in bits 0..8, `text_len` in bits 8..24.
    meta: AtomicU64,
}

impl Desc {
    const fn new() -> Self {
        Self {
            state_var: AtomicU64::new(0),
            begin_lpos: AtomicU64::new(0),
            ts_us: AtomicU64::new(0),
            meta: AtomicU64::new(0),
        }
    }
}

struct Prb {
    descs: [Desc; DESC_COUNT as usize],
    data: [AtomicU8; DATA_SIZE],
    /// Newest reserved descriptor id (0 = no records yet).
    head_id: AtomicU64,
    /// Oldest descriptor id still in the live window.
    tail_id: AtomicU64,
    /// Newest reserved data position.
    data_head: AtomicU64,
    /// Oldest data position still referenced.
    data_tail: AtomicU64,
    /// Records with id `<= clear_seq` are hidden from readers (syslog CLEAR).
    clear_seq: AtomicU64,
    /// Count of records dropped because the ring was momentarily full.
    dropped: AtomicU64,
}

static PRB: Prb = Prb {
    descs: [const { Desc::new() }; DESC_COUNT as usize],
    data: [const { AtomicU8::new(0) }; DATA_SIZE],
    head_id: AtomicU64::new(0),
    tail_id: AtomicU64::new(0),
    data_head: AtomicU64::new(0),
    data_tail: AtomicU64::new(0),
    clear_seq: AtomicU64::new(0),
    dropped: AtomicU64::new(0),
};

fn desc(id: u64) -> &'static Desc {
    &PRB.descs[(id & DESC_INDEX_MASK) as usize]
}

// ---- byte-ring access (positions are logical; physical = lpos & mask) ----

fn data_write_bytes(lpos: u64, src: &[u8]) {
    for (i, &b) in src.iter().enumerate() {
        let off = ((lpos + i as u64) & DATA_INDEX_MASK) as usize;
        PRB.data[off].store(b, Relaxed);
    }
}
fn data_read_bytes(lpos: u64, out: &mut [u8]) {
    for (i, b) in out.iter_mut().enumerate() {
        let off = ((lpos + i as u64) & DATA_INDEX_MASK) as usize;
        *b = PRB.data[off].load(Relaxed);
    }
}
fn data_write_u64(lpos: u64, v: u64) {
    data_write_bytes(lpos, &v.to_le_bytes());
}
fn data_read_u64(lpos: u64) -> u64 {
    let mut b = [0u8; 8];
    data_read_bytes(lpos, &mut b);
    u64::from_le_bytes(b)
}
fn data_write_u32(lpos: u64, v: u32) {
    data_write_bytes(lpos, &v.to_le_bytes());
}
fn data_read_u32(lpos: u64) -> u32 {
    let mut b = [0u8; 4];
    data_read_bytes(lpos, &mut b);
    u32::from_le_bytes(b)
}

// ---- writer ----

/// Reserve a fresh descriptor id, making room by retiring the oldest record(s).
/// Returns `None` if the oldest record is still being written (ring momentarily
/// full).
fn desc_reserve() -> Option<u64> {
    loop {
        let head = PRB.head_id.load(Relaxed);
        let id = head + 1;
        loop {
            let tail = PRB.tail_id.load(Relaxed);
            if id - tail < DESC_COUNT {
                break;
            }
            if !desc_push_tail(tail) {
                return None;
            }
        }
        if PRB
            .head_id
            .compare_exchange_weak(head, id, Release, Relaxed)
            .is_ok()
        {
            // We now exclusively own `id`. Its slot's previous occupant has been
            // retired (reusable) or was never used (0), so this CAS succeeds.
            let d = desc(id);
            let prev = d.state_var.load(Relaxed);
            if d
                .state_var
                .compare_exchange(prev, make_sv(id, ST_RESERVED), Acquire, Relaxed)
                .is_ok()
            {
                return Some(id);
            }
        }
    }
}

/// Retire descriptor `tail` if possible, advancing `tail_id` past it. Returns
/// `false` only when `tail` is still being written.
fn desc_push_tail(tail: u64) -> bool {
    let d = desc(tail);
    let sv = d.state_var.load(Acquire);
    match desc_state(tail, sv) {
        Some(ST_RESERVED) => return false,
        Some(ST_FINALIZED) => {
            let _ = d
                .state_var
                .compare_exchange(sv, make_sv(tail, ST_REUSABLE), Release, Relaxed);
        }
        // Reusable, or a miss (slot already recycled): nothing to retire.
        _ => {}
    }
    let _ = PRB.tail_id.compare_exchange(tail, tail + 1, Release, Relaxed);
    true
}

/// Reserve a data block of `text_len` bytes for record `id`, retiring old data
/// as needed. Returns the block's begin position, or `None` if the ring is full
/// of still-live data.
fn data_alloc(text_len: usize, id: u64) -> Option<u64> {
    let block = BLOCK_HDR + text_len as u64;
    if block > DATA_SIZE as u64 {
        return None;
    }
    loop {
        let head = PRB.data_head.load(Relaxed);
        let next = head + block;
        loop {
            let tail = PRB.data_tail.load(Relaxed);
            if next - tail <= DATA_SIZE as u64 {
                break;
            }
            if !data_push_tail(next - DATA_SIZE as u64) {
                return None;
            }
        }
        if PRB
            .data_head
            .compare_exchange_weak(head, next, Release, Relaxed)
            .is_ok()
        {
            data_write_u64(head, id);
            data_write_u32(head + 8, text_len as u32);
            return Some(head);
        }
    }
}

/// Advance `data_tail` until it reaches `target`, reclaiming whole blocks whose
/// owning descriptor is no longer live. Returns `false` if a still-live block
/// blocks the way.
fn data_push_tail(target: u64) -> bool {
    loop {
        let tail = PRB.data_tail.load(Relaxed);
        if tail >= target {
            return true;
        }
        let block_id = data_read_u64(tail);
        let text_len = data_read_u32(tail + 8) as u64;
        let block_next = tail + BLOCK_HDR + text_len;
        let sv = desc(block_id).state_var.load(Acquire);
        match desc_state(block_id, sv) {
            // Still-live data: the record is being written or is current.
            Some(ST_RESERVED) | Some(ST_FINALIZED) => return false,
            // Reusable, or a miss (descriptor recycled): the block is dead.
            _ => {}
        }
        let _ = PRB
            .data_tail
            .compare_exchange(tail, block_next, Release, Relaxed);
    }
}

/// Store one record with the given priority byte and microsecond timestamp.
pub fn push(priority: u8, ts_us: u64, msg: &str) {
    let text = msg.as_bytes();
    let text = &text[..text.len().min(MSG_MAX)];

    let Some(id) = desc_reserve() else {
        PRB.dropped.fetch_add(1, Relaxed);
        return;
    };
    let d = desc(id);

    match data_alloc(text.len(), id) {
        Some(begin) => {
            data_write_bytes(begin + BLOCK_HDR, text);
            d.begin_lpos.store(begin, Relaxed);
            d.ts_us.store(ts_us, Relaxed);
            d.meta
                .store(priority as u64 | ((text.len() as u64) << 8), Relaxed);
        }
        None => {
            // No data space; finalize as an empty record so the descriptor
            // stays consistent, and count the drop.
            PRB.dropped.fetch_add(1, Relaxed);
            d.begin_lpos.store(0, Relaxed);
            d.ts_us.store(ts_us, Relaxed);
            d.meta.store(priority as u64, Relaxed);
        }
    }

    // Publish: release the data and metadata stores above to readers.
    let _ = d
        .state_var
        .compare_exchange(make_sv(id, ST_RESERVED), make_sv(id, ST_FINALIZED), Release, Relaxed);
}

/// Format and store one kernel log record. Renders into a fixed stack buffer,
/// so it performs no heap allocation.
pub fn push_fmt(priority: u8, ts_us: u64, args: core::fmt::Arguments<'_>) {
    let mut msg = ByteBuf::<MSG_MAX>::new();
    let _ = core::fmt::write(&mut msg, args);
    push(priority, ts_us, msg.as_str());
}

// ---- reader ----

struct RecordView {
    ts_us: u64,
    priority: u8,
    text_len: usize,
    begin: u64,
}

/// Read a finalized record's metadata consistently. Returns `None` if the slot
/// is not a readable finalized record for `id`, or changed mid-read.
fn desc_read(id: u64) -> Option<RecordView> {
    let d = desc(id);
    let sv = d.state_var.load(Acquire);
    if desc_state(id, sv) != Some(ST_FINALIZED) {
        return None;
    }
    let ts_us = d.ts_us.load(Relaxed);
    let meta = d.meta.load(Relaxed);
    let begin = d.begin_lpos.load(Relaxed);
    if d.state_var.load(Acquire) != sv {
        return None;
    }
    Some(RecordView {
        ts_us,
        priority: (meta & 0xff) as u8,
        text_len: ((meta >> 8) & 0xffff) as usize,
        begin,
    })
}

/// Copy a record's text out, re-checking the descriptor afterwards to ensure the
/// data was not recycled during the copy. Returns the bytes copied.
fn read_text(id: u64, rv: &RecordView, out: &mut [u8]) -> Option<usize> {
    let n = rv.text_len.min(out.len());
    data_read_bytes(rv.begin + BLOCK_HDR, &mut out[..n]);
    if desc(id).state_var.load(Acquire) != make_sv(id, ST_FINALIZED) {
        return None;
    }
    Some(n)
}

/// Oldest record id a reader may return (respecting retirement and CLEAR).
fn read_floor() -> u64 {
    PRB.tail_id
        .load(Relaxed)
        .max(PRB.clear_seq.load(Relaxed) + 1)
        .max(1)
}

fn syslog_line_len(priority: u8, text_len: usize) -> usize {
    let pri_digits = if priority >= 100 {
        3
    } else if priority >= 10 {
        2
    } else {
        1
    };
    1 + pri_digits + 1 + text_len + 1
}

/// Render the most recent records as plain syslog text (`<pri>message\n` per
/// record) into `out`, newest prioritised when `out` cannot hold all. Returns
/// the number of bytes written. (`SYSLOG_ACTION_READ_ALL`.)
pub fn read_all(out: &mut [u8]) -> usize {
    let head = PRB.head_id.load(Relaxed);
    let floor = read_floor();
    let cap = out.len();

    let mut total = 0usize;
    for id in floor..=head {
        if let Some(rv) = desc_read(id) {
            total += syslog_line_len(rv.priority, rv.text_len);
        }
    }
    let mut to_skip = total.saturating_sub(cap);

    let mut text = [0u8; MSG_MAX];
    let mut w = 0usize;
    for id in floor..=head {
        let Some(rv) = desc_read(id) else { continue };
        let line_len = syslog_line_len(rv.priority, rv.text_len);
        if to_skip >= line_len {
            to_skip -= line_len;
            continue;
        }
        let Some(n) = read_text(id, &rv, &mut text) else {
            continue;
        };
        let mut line = ByteBuf::<{ MSG_MAX + 8 }>::new();
        let _ = core::fmt::write(
            &mut line,
            format_args!("<{}>{}\n", rv.priority, Bytes(&text[..n])),
        );
        let bytes = line.as_bytes();
        let m = bytes.len().min(cap - w);
        out[w..w + m].copy_from_slice(&bytes[..m]);
        w += m;
    }
    w
}

/// Result of a structured `/dev/kmsg` read.
pub struct KmsgRead {
    /// Sequence number of the returned record.
    pub seq: u64,
    /// Bytes written into the caller buffer.
    pub len: usize,
}

/// Read the first finalized record whose sequence number is `>= after_seq`,
/// formatted in Linux `/dev/kmsg` form `priority,seq,timestamp,flags;message\n`,
/// into `out`. Returns `None` when no such record exists yet.
pub fn read_record(after_seq: u64, out: &mut [u8]) -> Option<KmsgRead> {
    let head = PRB.head_id.load(Relaxed);
    let mut id = after_seq.max(read_floor());
    let mut text = [0u8; MSG_MAX];
    while id <= head {
        if let Some(rv) = desc_read(id)
            && let Some(n) = read_text(id, &rv, &mut text)
        {
            let mut line = ByteBuf::<{ MSG_MAX + 64 }>::new();
            let _ = core::fmt::write(
                &mut line,
                format_args!(
                    "{},{},{},-;{}\n",
                    rv.priority,
                    id,
                    rv.ts_us,
                    Bytes(&text[..n])
                ),
            );
            let bytes = line.as_bytes();
            let m = bytes.len().min(out.len());
            out[..m].copy_from_slice(&bytes[..m]);
            return Some(KmsgRead { seq: id, len: m });
        }
        id += 1;
    }
    None
}

/// Total bytes the current records would render to as syslog text
/// (`SYSLOG_ACTION_SIZE_UNREAD`).
pub fn text_len() -> usize {
    let head = PRB.head_id.load(Relaxed);
    let floor = read_floor();
    let mut total = 0usize;
    for id in floor..=head {
        if let Some(rv) = desc_read(id) {
            total += syslog_line_len(rv.priority, rv.text_len);
        }
    }
    total
}

/// Text data ring capacity in bytes (`SYSLOG_ACTION_SIZE_BUFFER`).
pub fn capacity() -> usize {
    DATA_SIZE
}

/// Hide all current records from future reads (`SYSLOG_ACTION_CLEAR`).
pub fn clear() {
    PRB.clear_seq.store(PRB.head_id.load(Relaxed), Relaxed);
}

/// One past the newest reserved record id. A reader whose cursor is `< latest_seq()`
/// may have records to read.
pub fn latest_seq() -> u64 {
    PRB.head_id.load(Relaxed) + 1
}

// ---- formatting helpers ----

/// `Display` adapter for stored record bytes (a complete `&str`, modulo
/// truncation at `MSG_MAX`) without allocating.
struct Bytes<'a>(&'a [u8]);

impl core::fmt::Display for Bytes<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(core::str::from_utf8(self.0).unwrap_or("<kmsg: invalid utf-8>"))
    }
}

/// Fixed-capacity, allocation-free byte buffer used to render a single line.
struct ByteBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> ByteBuf<N> {
    fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
    fn as_str(&self) -> &str {
        core::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Write for ByteBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let n = bytes.len().min(N - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&bytes[..n]);
        self.len += n;
        Ok(())
    }
}
