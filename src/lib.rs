#![no_std]

extern crate alloc;

mod process;
mod process_group;
mod session;

/// Process id.
pub type Pid = u32;
/// Process group id.
pub type Pgid = u32;
/// Session Id.
pub type Sid = u32;

pub use process::{Process, ProcessBuilder};
pub use process_group::ProcessGroup;
pub use session::Session;
