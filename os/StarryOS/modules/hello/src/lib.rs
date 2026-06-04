//! Minimal "hello world" loadable kernel module. Ported from
//! `Starry-OS/StarryOS:ebpf-kmod` (`modules/hello/src/lib.rs`), keeping the
//! original `vec![..]` + `{:?}` demo to exercise heap allocation and `Debug`
//! formatting from a loadable module.
//!
//! Loaded via `init_module(2)` / `finit_module(2)` (see
//! `os/StarryOS/kernel/src/syscall/kmod.rs`). Every symbol a module references
//! is resolved at relocation time by `KmodHelper::resolve_symbol` against the
//! in-kernel `.kallsyms`, so a module may only use kernel symbols that survive
//! into the final kernel image. The kernel is built in loadable-module mode
//! (`STARRY_KMOD=y`, `lto=false`) precisely so those symbols are retained — the
//! `core::fmt` Debug builders and the Rust allocator shims (incl. the
//! `__rust_no_alloc_shim_is_unstable_v2` guard) are *not* inlined/DCE'd away —
//! so `Vec` (heap) and `{:?}` (Debug) both resolve.

#![no_std]
extern crate alloc;

use alloc::vec;

use kmod_tools::{exit_fn, init_fn, module};

unsafe extern "C" {
    fn write_char(c: u8);
}

struct Writer;

impl core::fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for &b in s.as_bytes() {
            unsafe { write_char(b) };
        }
        Ok(())
    }
}

#[init_fn]
pub fn hello_init() -> i32 {
    let mut writer = Writer;
    let _ = core::fmt::write(&mut writer, format_args!("Hello, Kernel Module!\n"));
    // Heap allocation (`vec!`) + `{:?}` Debug — both resolve against the
    // `STARRY_KMOD` kernel's retained `.kallsyms` (allocator shims + `core::fmt`
    // Debug builders).
    let v = vec![1, 2, 3, 4, 5];
    let _ = core::fmt::write(&mut writer, format_args!("Vector contents: {v:?}\n"));
    0
}

#[exit_fn]
fn hello_exit() {
    let mut writer = Writer;
    let _ = core::fmt::write(&mut writer, format_args!("Goodbye, Kernel Module!\n"));
}

module!(
    name: "hello",
    license: "GPL",
    description: "A simple hello world kernel module",
    version: "0.1.0",
);
