//! Structured device metadata attached to a log record (`dev_printk_info`).
//!
//! These are the `SUBSYSTEM=`/`DEVICE=` fields a device-level log call carries
//! and that are exposed on `/dev/kmsg` continuation lines. They are a distinct
//! concern from the ring buffer itself; the record merely stores a copy.

/// Length of the `subsystem` field (`PRINTK_INFO_SUBSYSTEM_LEN`).
pub const SUBSYSTEM_LEN: usize = 16;
/// Length of the `device` field (`PRINTK_INFO_DEVICE_LEN`).
pub const DEVICE_LEN: usize = 48;

/// Structured device metadata (`dev_printk_info`). Both fields are fixed-size,
/// NUL-padded byte arrays.
#[derive(Clone, Copy)]
pub struct DevInfo {
    /// `SUBSYSTEM=` value.
    pub subsystem: [u8; SUBSYSTEM_LEN],
    /// `DEVICE=` value.
    pub device: [u8; DEVICE_LEN],
}

impl DevInfo {
    /// An all-zero (empty) `DevInfo`.
    pub const fn new() -> Self {
        Self {
            subsystem: [0; SUBSYSTEM_LEN],
            device: [0; DEVICE_LEN],
        }
    }
}
