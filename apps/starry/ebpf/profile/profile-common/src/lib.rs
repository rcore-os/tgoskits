#![no_std]

//! Shared ABI for the `profile` syscall-frequency profiler.
//!
//! The eBPF half attaches to Linux-compatible `raw_syscalls:sys_enter`. The
//! cooked tracepoint payload starts with the standard eight-byte common header,
//! followed by the signed syscall id and six native-width arguments.

/// Offset of `id` after the Linux-compatible tracepoint common header.
pub const SYSCALL_ID_OFFSET: usize = 8;
