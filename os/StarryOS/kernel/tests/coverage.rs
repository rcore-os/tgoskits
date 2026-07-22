#![cfg_attr(target_os = "none", no_std)]
#![cfg_attr(target_os = "none", no_main)]

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
#[path = "cases/axtest_fs.rs"]
mod axtest_fs;
#[path = "cases/axtest_memory.rs"]
mod axtest_memory;
#[path = "cases/axtest_rsext4.rs"]
mod axtest_rsext4;
#[path = "cases/axtest_runtime.rs"]
mod axtest_runtime;
#[path = "cases/axtest_syscall.rs"]
mod axtest_syscall;

#[axtest::tests]
mod tests {}
