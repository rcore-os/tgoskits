#![no_std]

//! Shared ABI knobs for the `profile` syscall-frequency profiler (demo D3).
//!
//! The eBPF half hangs a kprobe on the kernel's central syscall dispatcher
//! `starry_kernel::syscall::handle_syscall(uctx: &mut UserContext)`. On a
//! kprobe the program context is the trap-frame `pt_regs`, and on x86_64 the
//! first integer argument register (`rdi`) holds the `&UserContext` pointer.
//!
//! The syscall number is the saved user `rax`, which on x86_64 is the *first*
//! field of `TrapFrame`, itself the first field of `UserContext` — i.e. at
//! byte offset 0. So the program reads it with a single `bpf_probe_read` of 8
//! bytes at the `&UserContext` pointer, with no struct-layout offset to track.
//! This constant records that assumption in one place; bump it (and document
//! the arch) if the profiler is ever pointed at a non-x86_64 dispatcher whose
//! syscall-number register is not the first `TrapFrame` field.

/// Byte offset of the saved syscall-number register inside `UserContext`,
/// for the kprobe target `handle_syscall`. x86_64: `rax` is `TrapFrame`'s
/// first field, so offset 0.
pub const SYSNO_OFFSET_IN_USERCONTEXT: usize = 0;
