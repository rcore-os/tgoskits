#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(missing_docs)]
#![doc = include_str!("../README.md")]

#[macro_use]
extern crate log;

#[macro_use]
extern crate ax_memory_addr;

pub use ax_memory_addr::{MemoryAddr, PhysAddr, VirtAddr};

#[macro_use]
pub mod trap;

pub(crate) use trap::TrapOrigin;

#[cfg(feature = "context")]
mod task_local;
#[cfg(feature = "context")]
pub(crate) use task_local::TaskLocalState;

pub mod capability;

pub mod paging;

#[cfg(feature = "exception-table")]
mod exception_table;
#[cfg(feature = "uspace")]
mod user_access;
#[cfg(feature = "uspace")]
mod uspace_common;
#[cfg(feature = "uspace")]
pub(crate) use user_access::UserAccessType;

mod arch;
pub mod boot;
pub mod cache;
pub mod context;
pub mod interrupt;
pub mod mmu;
pub mod registers;
pub mod timer;

#[cfg(all(target_arch = "aarch64", feature = "pmu"))]
pub mod pmu;
pub(crate) use arch::current::asm;
#[cfg(feature = "uspace")]
pub(crate) use arch::current::uspace;

#[cfg(feature = "uspace")]
pub mod user;

pub mod barrier;

#[cfg(feature = "context")]
pub(crate) use context::KernelTlsBase;

#[cfg(feature = "virtualization")]
pub mod virtualization;
