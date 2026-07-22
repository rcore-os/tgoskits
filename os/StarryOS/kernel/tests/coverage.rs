#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]
#![feature(core_io)]
#![feature(core_io_borrowed_buf)]

extern crate alloc;

use ax_std as _;

#[path = "cases/axtest_ax_lazyinit.rs"]
mod axtest_ax_lazyinit;
#[path = "cases/axtest_axerrno.rs"]
mod axtest_axerrno;
#[path = "cases/axtest_axfs_ng_vfs.rs"]
mod axtest_axfs_ng_vfs;
#[path = "cases/axtest_axio.rs"]
mod axtest_axio;
#[path = "cases/axtest_axpoll.rs"]
mod axtest_axpoll;
#[path = "cases/axtest_core_utils.rs"]
mod axtest_core_utils;
#[path = "cases/axtest_fs.rs"]
mod axtest_fs;
#[path = "cases/axtest_kernel_guard.rs"]
mod axtest_kernel_guard;
#[path = "cases/axtest_memory.rs"]
mod axtest_memory;
#[path = "cases/axtest_rsext4.rs"]
mod axtest_rsext4;
#[path = "cases/axtest_runtime.rs"]
mod axtest_runtime;
#[path = "cases/axtest_scope_local.rs"]
mod axtest_scope_local;
#[path = "cases/axtest_starry_vm.rs"]
mod axtest_starry_vm;
#[path = "cases/axtest_syscall.rs"]
mod axtest_syscall;

#[axtest::tests]
mod tests {}
