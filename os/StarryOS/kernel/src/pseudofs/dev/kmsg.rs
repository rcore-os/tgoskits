//! `/dev/kmsg` — kernel log device (char major 1, minor 11).
//!
//! Write side only: a writer emits log records here before any userspace
//! logger exists. Each `write()` is one record; we parse its optional
//! `<priority>` prefix the way Linux's `devkmsg_write()` does and forward the
//! message to the kernel log so it reaches the console.
//!
//! The read side (history replay) is not implemented yet; `read_at` returns
//! EOF.

use core::any::Any;

use axfs_ng_vfs::{NodeFlags, VfsResult};

use crate::pseudofs::DeviceOps;

/// Level used when a record carries no valid `<priority>` prefix. Linux falls
/// back to `default_message_loglevel`; `6` (`LOG_INFO`) is the closest sane
/// default for forwarded userspace messages.
const DEFAULT_LEVEL: u8 = 6;

/// `/dev/kmsg` device. Stateless: like Linux, each `write()` is one record.
pub(crate) struct Kmsg;

impl Kmsg {
    /// Split a kmsg record into `(severity, message)`.
    ///
    /// Mirrors Linux `devkmsg_write()`: if the record starts with `<`, parse
    /// the following decimal digits as the syslog priority
    /// (`facility << 3 | severity`) and accept it only when a `>` immediately
    /// follows the digits. The severity is the low 3 bits. Anything else (no
    /// `<`, no digits, or a non-`>` terminator) is not a prefix and the whole
    /// record is the message with the default level.
    fn parse(buf: &[u8]) -> (u8, &[u8]) {
        if let [b'<', rest @ ..] = buf {
            let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            if digits > 0 && rest.get(digits) == Some(&b'>') {
                let mut priority: u64 = 0;
                for &c in &rest[..digits] {
                    priority = priority
                        .saturating_mul(10)
                        .saturating_add((c - b'0') as u64);
                }
                return ((priority & 7) as u8, &rest[digits + 1..]);
            }
        }
        (DEFAULT_LEVEL, buf)
    }
}

impl DeviceOps for Kmsg {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        let (severity, msg) = Self::parse(buf);
        // The record layer (here, axlog) terminates each line, so drop a
        // single trailing newline to avoid emitting a blank line after it.
        let msg = msg.strip_suffix(b"\n").unwrap_or(msg);
        let text = core::str::from_utf8(msg).unwrap_or("<kmsg: invalid utf-8>");
        match severity {
            // EMERG / ALERT / CRIT / ERR
            0..=3 => error!("{text}"),
            // WARNING
            4 => warn!("{text}"),
            // NOTICE / INFO
            5 | 6 => info!("{text}"),
            // DEBUG (and anything out of range)
            _ => debug!("{text}"),
        }
        // Always report the whole record consumed, as Linux does.
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }
}
