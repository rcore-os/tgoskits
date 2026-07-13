//! # The ArceOS Standard Library
//!
//! The [ArceOS] Standard Library is a mini-std library, with an interface similar
//! to rust [std], but calling the functions directly in ArceOS modules, instead
//! of using libc and system calls.
//!
//! ## Cargo Features
//!
//! - CPU
//!     - `smp`: Enable SMP (symmetric multiprocessing) support.
//!     - `fp-simd`: Enable floating point and SIMD support.
//! - Interrupts:
//!     - `irq`: Enable interrupt handling support.
//! - Memory
//!     - `alloc`: Enable dynamic memory allocation.
//!     - `paging`: Enable page table manipulation.
//!     - `tls`: Enable thread-local storage.
//! - Task management
//!     - `multitask`: Enable multi-threading support.
//!     - `sched-rr`: Use the Round-robin preemptive scheduler.
//!     - `sched-cfs`: Use the Completely Fair Scheduler (CFS) preemptive scheduler.
//! - Upperlayer stacks
//!     - `fs`: Enable file system support.
//!     - `ext4fs`: Enable the ext4 filesystem.
//!     - `fatfs`: Enable the FAT filesystem.
//!     - `net`: Enable networking support.
//!     - `dns`: Enable DNS lookup support.
//!     - `display`: Enable graphics support.
//! - Device drivers are selected directly through `ax-driver/*` features by
//!   board configurations.
//!
//! [ArceOS]: https://github.com/arceos-org/arceos

#![cfg_attr(all(not(test), not(doc)), no_std)]
#![cfg_attr(doc, feature(doc_cfg))]

extern crate alloc as alloc_crate;
extern crate ax_driver as _;

/// Memory-allocation APIs compatible with [`std::alloc`].
pub mod alloc {
    pub use core::alloc::{GlobalAlloc, Layout, LayoutError};

    pub use ::alloc_crate::alloc::{alloc, alloc_zeroed, dealloc, handle_alloc_error, realloc};
}

#[doc(no_inline)]
pub use core::{arch, cell, cmp, hint, marker, mem, ops, ptr, slice, str};

#[cfg(feature = "alloc")]
#[doc(no_inline)]
pub use alloc_crate::{boxed, collections, format, string, vec};

#[macro_use]
mod macros;

pub mod env;
pub mod io;
pub mod os;
pub mod process;
pub mod sync;
pub mod thread;
pub mod time;

#[cfg(feature = "fs")]
pub mod fs;
#[cfg(feature = "net")]
pub mod net;
