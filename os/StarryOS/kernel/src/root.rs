extern crate alloc;
extern crate ax_runtime;

#[macro_use]
extern crate ax_log;

#[macro_use]
pub mod dyn_debug; // Re-export debug macros for use in other modules. It will override the `debug` macro from `log` crate when `dynamic_debug` feature is enabled.

pub mod entry;

#[cfg(all(test, not(axtest)))]
mod host_link_symbols {
    // Host unit tests do not execute the bare-metal boot path, but someboot's
    // linked API refers to linker-script symbols even when those functions are
    // dead code. Provide inert symbols so the standard test harness can link;
    // runtime semantics remain covered only by the target axtest binary.
    #[unsafe(no_mangle)]
    static STACK_SIZE: usize = 0;
    #[unsafe(no_mangle)]
    static PAGE_SIZE: usize = 0;
    #[unsafe(no_mangle)]
    static __PERCPU_TEMPLATE_ALIGN_START: usize = 0;
    #[unsafe(no_mangle)]
    static __PERCPU_TEMPLATE_ALIGN_END: usize = 0;
}

mod cgroup;
mod config;
mod ebpf;
mod error;
mod file;
mod ipc;
mod kmod;
pub mod kprobe;
mod mm;
mod namespace;
mod perf;
mod pseudofs;
mod stop_machine;
mod sync;
mod syscall;
mod task;
mod time;
mod tracepoint;
mod trap;
mod uprobe;

pub use error::{DmaOperation, StarryError, StarryResult};
pub use syscalls::Errno;
