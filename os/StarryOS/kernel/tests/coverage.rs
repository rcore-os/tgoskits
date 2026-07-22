#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![feature(core_io)]
#![feature(core_io_borrowed_buf)]

extern crate alloc;

use ax_cpumask as _;
use ax_errno as _;
use ax_io as _;
use ax_kernel_guard as _;
use ax_lazyinit as _;
use ax_std as _;
use axfs_ng_vfs as _;
use axpoll as _;
use kernutil as _;
use rsext4 as _;
use scope_local as _;

#[path = "cases/axtest_fs.rs"]
mod axtest_fs;
#[path = "cases/axtest_memory.rs"]
mod axtest_memory;
#[path = "cases/axtest_runtime.rs"]
mod axtest_runtime;
#[path = "cases/axtest_starry_vm.rs"]
mod axtest_starry_vm;
#[path = "cases/axtest_syscall.rs"]
mod axtest_syscall;

#[axtest::tests]
mod tests {}
