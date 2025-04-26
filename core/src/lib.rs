//! The core functionality of a monolithic kernel, including loading user
//! programs and managing processes.

#![no_std]
#![warn(missing_docs)]

#[macro_use]
extern crate axlog;
extern crate alloc;

pub mod mm;
pub mod task;
mod time;
