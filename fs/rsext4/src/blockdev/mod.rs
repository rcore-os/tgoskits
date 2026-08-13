//! Block device abstractions, buffering, and JBD2 integration.

mod buffer;
mod cached_device;
mod journal;

pub use buffer::BlockBuffer;
pub use journal::{Jbd2Dev, Jbd2RunState};
pub(crate) use journal::{ReservedJournalHandle, TransactionCredits, TransactionHandleExtension};

pub use crate::io::BlockIo;
use crate::{bmalloc::AbsoluteBN, error::Ext4Result, io::WriteFlags};

/// Private filesystem-block I/O used by ext4 and JBD2 after sector mapping.
pub(crate) trait FilesystemBlockIo {
    fn block_size(&self) -> usize;
    fn read(&mut self, buffer: &mut [u8], block: AbsoluteBN, count: u32) -> Ext4Result<()>;
    fn write(&mut self, buffer: &[u8], block: AbsoluteBN, count: u32) -> Ext4Result<()>;
    fn write_with_flags(
        &mut self,
        buffer: &[u8],
        block: AbsoluteBN,
        count: u32,
        flags: WriteFlags,
    ) -> Ext4Result<()>;
    fn flush(&mut self) -> Ext4Result<()>;
}
