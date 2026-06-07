#![no_std]

//! Shared crate for the `profile` syscall-frequency profiler (demo D3).
//!
//! The eBPF half hangs a kprobe on `starry_kernel::syscall::sysno(id: usize)`,
//! the `#[inline(never)]` helper `handle_syscall` calls once per syscall with
//! the raw syscall number as its first argument. On a kprobe the program
//! context is the trap-frame `pt_regs`, and `ctx.arg(0)` reads the first
//! integer argument register (rdi / x0 / a0 / a0) — i.e. the syscall number
//! itself — with no dereference and no per-arch `TrapFrame` field offset to
//! track. That keeps the program correct on all supported arches, so this
//! crate carries no ABI offsets; it stays as the shared `-common` placeholder
//! matching the other demos.
