//! High-level kernel log ring: the public read/write view over the lockless
//! [`ax_printk`] ring buffer.
//!
//! `ax_printk` provides the low-level reserve/commit/read primitives; this module
//! renders them into the two consumer-facing shapes:
//!
//! - the **write side** ([`push`], [`push_fmt`]) — reserve a record, copy the
//!   text into the zero-copy buffer, and finalize it;
//! - the **syslog view** ([`read_all`], [`text_len`], [`capacity`], [`clear`])
//!   — the `SYSLOG_ACTION_*` semantics, rendering records as `<pri>message\n`;
//! - the **`/dev/kmsg` view** ([`read_record`]) — one record rendered as
//!   `pri,seq,ts_usec,flags;message\n`.
//!
//! The `SYSLOG_ACTION_CLEAR` floor is tracked here (`CLEAR_SEQ`), not in `prb`:
//! it is a property of the syslog reader, not of the ring buffer itself.

// TEMPORARILY DISABLED while `ax-printk` is being rewritten (Blocks 2-7).
// Re-enable once its public API (prb_reserve/set_info/read_valid/first_valid_seq/
// next_seq/capacity/ReadInfo/...) is available again.
/*
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use ax_printk::{self as prb, ReadInfo};

/// Maximum stored message length. Longer messages are truncated on store.
pub const MSG_MAX: usize = prb::MSG_MAX;

/// `SYSLOG_ACTION_CLEAR` floor: records with `seq < CLEAR_SEQ` are hidden from
/// syslog reads. Advanced to one-past-newest by [`clear`].
static CLEAR_SEQ: AtomicU64 = AtomicU64::new(0);

// ---- writer -----------------------------------------------------------------

/// Store one kernel log record with the given syslog `priority`
/// (`facility << 3 | level`) and monotonic timestamp `ts_nsec`.
pub fn push(priority: u8, ts_nsec: u64, msg: &str) {
    let text = msg.as_bytes();
    let text = &text[..text.len().min(MSG_MAX)];

    // `prb_reserve` handles a full ring (recycling) and out-of-space internally,
    // returning `None` (and bumping its own fail counter) when it cannot store.
    let Some((e, ptr, size)) = prb::prb_reserve(text.len()) else {
        return;
    };
    if size != 0 && !ptr.is_null() {
        let n = text.len().min(size);
        // SAFETY: `ptr` is the writer-exclusive text buffer of the reserved
        // record, valid for `size >= n` bytes; `text` is a distinct slice.
        unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), ptr, n) };
    }
    prb::set_info(&e, ts_nsec, priority, 0, text.len());
    prb::prb_final_commit(&e);
}

/// Format and store one kernel log record. Renders into a fixed stack buffer,
/// so it performs no heap allocation.
pub fn push_fmt(priority: u8, ts_nsec: u64, args: core::fmt::Arguments<'_>) {
    let mut msg = ByteBuf::<MSG_MAX>::new();
    let _ = core::fmt::write(&mut msg, args);
    push(priority, ts_nsec, msg.as_str());
}

// ---- reader -----------------------------------------------------------------

/// Oldest sequence number a reader may return, respecting record retirement and
/// `SYSLOG_ACTION_CLEAR`.
fn read_floor() -> u64 {
    prb::first_valid_seq().max(CLEAR_SEQ.load(Relaxed))
}

/// Bytes one record renders to as syslog text: `<pri>message\n`.
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

/// Total bytes the current (unread, non-cleared) records render to as syslog
/// text (`SYSLOG_ACTION_SIZE_UNREAD`).
pub fn text_len() -> usize {
    let end = prb::next_seq();
    let mut info = ReadInfo::default();
    let mut seq = read_floor();
    let mut total = 0usize;
    while seq < end {
        if prb::read_valid(seq, &mut info, None).is_none() {
            break;
        }
        total += syslog_line_len(info.priority, info.text_len);
        seq = info.seq + 1;
    }
    total
}

/// Render the retained records as plain syslog text (`<pri>message\n` per
/// record) into `out`, dropping the oldest first when `out` cannot hold all.
/// Returns the number of bytes written (`SYSLOG_ACTION_READ_ALL`).
pub fn read_all(out: &mut [u8]) -> usize {
    let cap = out.len();
    let floor = read_floor();
    let end = prb::next_seq();

    // Pass 1: total rendered size, to know how much of the oldest to drop.
    let mut info = ReadInfo::default();
    let mut seq = floor;
    let mut total = 0usize;
    while seq < end {
        if prb::read_valid(seq, &mut info, None).is_none() {
            break;
        }
        total += syslog_line_len(info.priority, info.text_len);
        seq = info.seq + 1;
    }
    let mut to_skip = total.saturating_sub(cap);

    // Pass 2: render, skipping whole oldest lines until the rest fits.
    let mut text = [0u8; MSG_MAX];
    let mut seq = floor;
    let mut w = 0usize;
    while seq < end && w < cap {
        let Some(n) = prb::read_valid(seq, &mut info, Some(&mut text)) else {
            break;
        };
        seq = info.seq + 1;
        let line_len = syslog_line_len(info.priority, info.text_len);
        if to_skip >= line_len {
            to_skip -= line_len;
            continue;
        }
        let mut line = ByteBuf::<{ MSG_MAX + 8 }>::new();
        let _ = core::fmt::write(
            &mut line,
            format_args!("<{}>{}\n", info.priority, Bytes(&text[..n])),
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

/// Read the first retained record whose sequence number is `>= after_seq`,
/// formatted in `/dev/kmsg` form `pri,seq,ts_usec,flags;message\n`, into `out`.
/// Returns `None` when no such record exists yet.
pub fn read_record(after_seq: u64, out: &mut [u8]) -> Option<KmsgRead> {
    let start = after_seq.max(read_floor());
    let mut info = ReadInfo::default();
    let mut text = [0u8; MSG_MAX];
    let n = prb::read_valid(start, &mut info, Some(&mut text))?;

    let mut line = ByteBuf::<{ MSG_MAX + 64 }>::new();
    let _ = core::fmt::write(
        &mut line,
        format_args!(
            "{},{},{},-;{}\n",
            info.priority,
            info.seq,
            info.ts_nsec / 1000,
            Bytes(&text[..n])
        ),
    );
    let bytes = line.as_bytes();
    let m = bytes.len().min(out.len());
    out[..m].copy_from_slice(&bytes[..m]);
    Some(KmsgRead { seq: info.seq, len: m })
}

/// Text data ring capacity in bytes (`SYSLOG_ACTION_SIZE_BUFFER`).
pub fn capacity() -> usize {
    prb::DATA_SIZE
}

/// Hide all current records from future syslog reads (`SYSLOG_ACTION_CLEAR`).
pub fn clear() {
    CLEAR_SEQ.store(prb::next_seq(), Relaxed);
}

/// One past the newest readable record. A reader whose cursor is `< latest_seq()`
/// may have records to read.
pub fn latest_seq() -> u64 {
    prb::next_seq()
}

// ---- formatting helpers -----------------------------------------------------

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
*/
