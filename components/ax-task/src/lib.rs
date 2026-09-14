//! OS-independent task scheduling primitives.
//!
//! The crate owns no global scheduler state. Operating systems create an explicit
//! [`runtime::TaskSystem`] and one pinned [`runtime::cpu::CpuLocal`] object for every online CPU.

#![no_std]
#![feature(allocator_api)]

extern crate alloc;
extern crate self as ax_task;

#[cfg(any(test, all(feature = "host-test", not(target_os = "none"))))]
extern crate std;

pub mod executor;

pub mod runtime;

pub mod sync;

pub mod thread;

pub mod diagnostics;
pub mod sched;
pub mod time;
