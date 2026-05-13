//! Block device abstractions, buffering, and JBD2 integration.

mod buffer;
mod cached_device;
mod journal;
mod traits;

pub use buffer::BlockBuffer;
pub(crate) use cached_device::{read_bytes_from_device, read_ext4_blocks, write_ext4_blocks};
pub use journal::{Jbd2Dev, Jbd2RunState};
pub use traits::{BlockDevice, DevBN, INeedBlockdevToWrite};
