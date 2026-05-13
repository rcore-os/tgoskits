//! PIO data-phase transfers.
//!
//! Reads from / writes to `REG_BUFFER_DATA_PORT` 32 bits at a time,
//! after waiting for the appropriate Buffer Read/Write Ready interrupt
//! flag, then waits for Transfer Complete.

use sdmmc_protocol::error::{Error, ErrorContext, Phase};

use crate::{host::Sdhci, regs::*};

const POLL_LIMIT: u32 = 1_000_000;

impl Sdhci {
    pub(crate) fn pio_read(
        &mut self,
        buf: &mut [u8],
        block_size: u32,
        cmd_index: u8,
    ) -> Result<(), Error> {
        let block_size = block_size as usize;
        if !buf.len().is_multiple_of(block_size) {
            return Err(Error::Misaligned);
        }

        for chunk in buf.chunks_mut(block_size) {
            self.wait_buffer_ready(true, cmd_index)?;
            for word_chunk in chunk.chunks_mut(4) {
                let word = self.read_u32(REG_BUFFER_DATA_PORT);
                let bytes = word.to_le_bytes();
                for (i, b) in word_chunk.iter_mut().enumerate() {
                    *b = bytes[i];
                }
            }
        }

        self.wait_data_complete(cmd_index)
    }

    pub(crate) fn pio_write(
        &mut self,
        buf: &[u8],
        block_size: u32,
        cmd_index: u8,
    ) -> Result<(), Error> {
        let block_size = block_size as usize;
        if !buf.len().is_multiple_of(block_size) {
            return Err(Error::Misaligned);
        }

        for chunk in buf.chunks(block_size) {
            self.wait_buffer_ready(false, cmd_index)?;
            for word_chunk in chunk.chunks(4) {
                let mut bytes = [0u8; 4];
                for (i, b) in word_chunk.iter().enumerate() {
                    bytes[i] = *b;
                }
                let word = u32::from_le_bytes(bytes);
                self.write_u32(REG_BUFFER_DATA_PORT, word);
            }
        }

        self.wait_data_complete(cmd_index)
    }

    fn wait_buffer_ready(&mut self, read: bool, cmd_index: u8) -> Result<(), Error> {
        let success = if read {
            NORMAL_INT_BUFFER_READ_READY
        } else {
            NORMAL_INT_BUFFER_WRITE_READY
        };
        let phase = if read {
            Phase::DataRead
        } else {
            Phase::DataWrite
        };
        for _ in 0..POLL_LIMIT {
            let status = self.read_u16(REG_NORMAL_INT_STATUS);
            if status & success != 0 {
                self.write_u16(REG_NORMAL_INT_STATUS, success);
                return Ok(());
            }
            if status & NORMAL_INT_ERROR != 0 {
                self.log_status("data buffer error", cmd_index);
                let err = self.read_u16(REG_ERROR_INT_STATUS);
                self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
                self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
                let _ = self.reset_cmd();
                let _ = self.reset_dat();
                let ctx = ErrorContext::for_cmd(phase, cmd_index);
                return Err(
                    if err & (ERROR_INT_DATA_TIMEOUT | ERROR_INT_CMD_TIMEOUT) != 0 {
                        Error::Timeout(ctx)
                    } else if err & (ERROR_INT_DATA_CRC | ERROR_INT_CMD_CRC) != 0 {
                        Error::Crc(ctx)
                    } else if read {
                        Error::ReadError(ctx)
                    } else {
                        Error::WriteError(ctx)
                    },
                );
            }
            core::hint::spin_loop();
        }
        self.log_status("data buffer wait timed out", cmd_index);
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
        let _ = self.reset_cmd();
        let _ = self.reset_dat();
        Err(Error::Timeout(ErrorContext::for_cmd(phase, cmd_index)))
    }
}
