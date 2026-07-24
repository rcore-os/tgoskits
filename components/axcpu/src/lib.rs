#![cfg_attr(not(test), no_std)]
#![cfg_attr(doc, feature(doc_cfg))]
#![feature(extern_item_impls)]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]

#[cfg(all(feature = "uspace", feature = "tls"))]
compile_error!("ax-cpu userspace requires LinuxCurrent and cannot enable kernel TLS mode");

#[macro_use]
extern crate log;

#[macro_use]
extern crate ax_memory_addr;

/// Host stage-1 page-table formats and operations.
pub mod paging;

#[macro_use]
pub mod trap;

pub use trap::TrapOrigin;

mod task_local;
pub use task_local::TaskLocalState;

pub mod cap;

/// Kernel task-local storage base owned by one execution context.
///
/// This value follows a task across CPUs. It must never be used as a CPU-local
/// anchor or initialized from an architecture per-CPU register.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KernelTlsBase(usize);

impl KernelTlsBase {
    /// Creates a kernel TLS base from its virtual address.
    pub const fn new(address: usize) -> Self {
        Self(address)
    }

    /// Returns the virtual address represented by this TLS base.
    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub(crate) fn for_task_context(requested: Self) -> Self {
        if cfg!(feature = "tls") {
            requested
        } else {
            assert!(
                requested.0 == 0,
                "LinuxCurrent task contexts must not own a kernel TLS register"
            );
            Self(0)
        }
    }
}

#[cfg(feature = "exception-table")]
mod exception_table;
#[cfg(feature = "uspace")]
mod uspace_common;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "x86_64")] {
        mod x86_64;
        pub use self::x86_64::*;
    } else if #[cfg(any(target_arch = "riscv32", target_arch = "riscv64"))] {
        mod riscv;
        pub use self::riscv::*;
    } else if #[cfg(target_arch = "aarch64")]{
        mod aarch64;
        pub use self::aarch64::*;
    } else if #[cfg(any(target_arch = "loongarch64"))] {
        mod loongarch64;
        pub use self::loongarch64::*;
    }
}
