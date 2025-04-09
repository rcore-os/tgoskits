//! Process Management

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod process;
mod process_group;
mod session;
mod thread;

/// A process ID, also used as session ID, process group ID, and thread ID.
pub type Pid = u32;

pub use process::{Process, ProcessBuilder, init_proc};
pub use process_group::ProcessGroup;
pub use session::Session;
pub use thread::{Thread, ThreadBuilder};
