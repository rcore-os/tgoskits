use super::{Error, Result};

/// Read access to a complete device using its logical block size and capacity.
pub trait BlockReader {
    /// Returns bytes per logical device block, shared by every access below.
    fn block_size(&self) -> usize;
    /// Returns the exclusive upper bound of valid logical block addresses.
    fn num_blocks(&self) -> u64;
    /// Reads device LBA `block` into exactly one block-sized `buf`.
    /// Backends report bounds, buffer and I/O failures through `Result`.
    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> Result<()>;

    /// Reads `blocks` consecutive blocks beginning at `start_block` into `buf`.
    /// Requires exactly `blocks * block_size()` output bytes and an in-range
    /// span; stops at the first read error, possibly leaving a partial output.
    fn read_blocks(&mut self, start_block: u64, blocks: u64, buf: &mut [u8]) -> Result<()> {
        let block_size = self.block_size();
        if block_size == 0 {
            return Err(Error::InvalidBlockSize);
        }
        let block_count = usize::try_from(blocks).map_err(|_| Error::OutOfRange)?;
        if buf.len()
            != block_size
                .checked_mul(block_count)
                .ok_or(Error::OutOfRange)?
        {
            return Err(Error::BufferSizeMismatch);
        }
        if start_block
            .checked_add(blocks)
            .is_none_or(|end| end > self.num_blocks())
        {
            return Err(Error::OutOfRange);
        }

        for (idx, block_buf) in buf.chunks_exact_mut(block_size).enumerate() {
            self.read_block(start_block + idx as u64, block_buf)?;
        }
        Ok(())
    }
}

/// Exclusive writable block access for partition-table management.
/// `flush` must make all preceding successful writes durable. Read-only scans
/// only require `BlockReader` and never acquire this capability.
pub trait BlockWriter: BlockReader {
    /// Writes one complete device block; success need not imply durability yet.
    /// `block` is a device LBA and `buffer` must contain `block_size()` bytes.
    /// Failure may leave partial media changes and must not be treated as rollback.
    fn write_block(&mut self, block: u64, buffer: &[u8]) -> Result<()>;
    /// Makes preceding successful writes durable before a later phase begins.
    /// Failure leaves durability uncertain and must abort the current update.
    fn flush(&mut self) -> Result<()>;
}
