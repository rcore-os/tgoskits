//! Block device abstractions, buffering, and JBD2 integration.

mod buffer;
mod cached_device;
mod journal;
mod traits;

pub use buffer::BlockBuffer;
pub use journal::{Jbd2Dev, Jbd2RunState};
pub use traits::{BlockDevice, INeedBlockdevToWrite};
