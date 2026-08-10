//! Block device abstractions, buffering, and JBD2 integration.

mod buffer;
mod cached_device;
mod journal;

pub use buffer::BlockBuffer;
pub use journal::{Jbd2Dev, Jbd2RunState};

pub use crate::io::BlockIo;
