//! RISC-V paging formats.

mod stage1;

pub use stage1::{DescriptorFlags, Pte};
pub(crate) use stage1::{LEVEL_BITS, MAX_BLOCK_LEVEL, PAGE_SIZE};
