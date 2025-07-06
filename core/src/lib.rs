//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

#[macro_use]
extern crate axlog;

pub mod futex;
pub mod mm;
pub mod resources;
pub mod task;
mod time;
pub mod vfs;

pub use time::ITimerType;
