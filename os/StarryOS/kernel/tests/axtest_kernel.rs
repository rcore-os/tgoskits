#![cfg_attr(target_os = "none", no_std)]
#![no_main]

extern crate alloc;

use ax_hal as _;
use ax_runtime as _;
use ax_std as _;

#[path = "cases/axtest_fs.rs"]
mod axtest_fs;
#[path = "cases/axtest_memory.rs"]
mod axtest_memory;
#[path = "cases/axtest_runtime.rs"]
mod axtest_runtime;
#[path = "cases/axtest_syscall.rs"]
mod axtest_syscall;

#[axtest::tests]
mod tests {}
