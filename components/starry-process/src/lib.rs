//! Process Management

#![no_std]
#![warn(missing_docs)]

extern crate alloc;

mod process;
mod process_group;
mod session;

/// A process ID, also used as session ID, process group ID, and thread ID.
pub type Pid = u32;

pub use process::{Process, init_proc};
pub use process_group::ProcessGroup;
pub use session::Session;
