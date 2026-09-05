#![no_std]

/// Offset of `id` after the Linux-compatible tracepoint common header.
pub const SYSCALL_ID_OFFSET: usize = 8;
