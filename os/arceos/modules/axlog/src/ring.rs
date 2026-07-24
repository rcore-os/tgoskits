//! Kernel-log rendering helpers (syslog / `/dev/kmsg` line formats).
//!
//! NOTE: the ring-backed read/write API (`push`/`read_all`/`read_record`/…) was
//! temporarily removed together with the `ax-printk` dependency. Only the
//! formatting helpers that do not depend on the ring buffer remain here; the
//! ring-backed API will be restored once this branch is stacked on `ax-printk`.
#![allow(dead_code)]

/// Result of a structured `/dev/kmsg` read.
pub struct KmsgRead {
    /// Sequence number of the returned record.
    pub seq: u64,
    /// Bytes written into the caller buffer.
    pub len: usize,
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
