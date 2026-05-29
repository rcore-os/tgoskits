use super::{Error, Result};

pub trait BlockReader {
    fn block_size(&self) -> usize;
    fn num_blocks(&self) -> u64;
    fn read_block(&mut self, block: u64, buf: &mut [u8]) -> Result<()>;

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
