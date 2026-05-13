//! PIO data-phase transfers over the DW_mshc data FIFO.
//!
//! Reads / writes 64-bit words to the FIFO mapped at
//! `base + fifo_offset` (0x200 by default). The protocol layer drives
//! one block at a time: `prepare_data_transfer` programs total
//! BYTCNT, the corresponding command kicks off the data phase, and
//! [`DwMmc::pio_read`] / [`DwMmc::pio_write`] then drain or fill
//! `block_size` bytes per call.
//!
//! Flow control: [`crate::regs::Status::fifo_empty`] / `fifo_full`
//! gate per-word access, so the code is portable across FIFO depths
//! (Rockchip/Allwinner variants use 256-word, StarFive uses 128-word,
//! etc.). Errors raised via [`crate::regs::RIntSts::error`] are
//! translated through [`DwMmc::translate_int_error`] and the FIFO is
//! reset before propagating so the next transfer starts clean.

use sdmmc_protocol::error::{Error, ErrorContext, Phase};

use crate::{host::DwMmc, regs::RegisterBlockVolatileFieldAccess};

const POLL_LIMIT: u32 = 8_000_000;

impl DwMmc {
    /// Drain `buf.len()` bytes from the FIFO. Caller must ensure
    /// `buf.len() % 8 == 0` (every callsite from the SDIO protocol
    /// layer uses 64-byte or 512-byte blocks, both multiples of 8).
    pub(crate) fn pio_read(
        &mut self,
        buf: &mut [u8],
        cmd_index: u8,
        wait_for_transfer_over: bool,
    ) -> Result<(), Error> {
        if !buf.len().is_multiple_of(8) {
            return Err(Error::Misaligned);
        }
        let fifo = self.fifo_ptr();
        let mut offset = 0usize;
        let mut idle_polls = 0u32;

        loop {
            let rintsts = self.regs.rintsts().read();
            if rintsts.error() {
                let err = self.translate_int_error(rintsts, Phase::DataRead, cmd_index);
                let _ = self.reset_fifo();
                return Err(err);
            }

            let mut drained_any = false;
            let mut status = self.regs.status().read();
            if rintsts.receive_fifo_data_request()
                || status.fifo_count() >= 2
                || (rintsts.data_transfer_over() && status.fifo_count() > 0)
            {
                while status.fifo_count() >= 2 {
                    let v = unsafe { fifo.read_volatile() };
                    if offset + 8 <= buf.len() {
                        buf[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
                    }
                    offset += 8;
                    drained_any = true;
                    status = self.regs.status().read();
                }
            }

            if offset >= buf.len() && (!wait_for_transfer_over || rintsts.data_transfer_over()) {
                break;
            }

            if drained_any {
                idle_polls = 0;
            } else {
                idle_polls = idle_polls.saturating_add(1);
                if idle_polls >= POLL_LIMIT {
                    return Err(Error::Timeout(ErrorContext::for_cmd(
                        Phase::DataRead,
                        cmd_index,
                    )));
                }
                core::hint::spin_loop();
            }
        }
        if wait_for_transfer_over {
            self.wait_data_transfer_over(cmd_index, Phase::DataRead)?;
            let mut ack = crate::regs::RIntSts::new();
            ack.set_data_transfer_over(true);
            self.regs.rintsts().write(ack);
        }
        Ok(())
    }

    /// Push `buf.len()` bytes into the FIFO, then wait for the
    /// controller to drain them and assert
    /// [`crate::regs::RIntSts::data_transfer_over`].
    ///
    /// Writes are gated on `data_transfer_over` (not just
    /// `fifo_empty`) because the FIFO can drain to empty *before*
    /// the bus has finished clocking the bytes out — racing the next
    /// command in past that boundary corrupts the write.
    pub(crate) fn pio_write(&mut self, buf: &[u8], cmd_index: u8) -> Result<(), Error> {
        if !buf.len().is_multiple_of(8) {
            return Err(Error::Misaligned);
        }
        let fifo = self.fifo_ptr();
        let mut offset = 0usize;
        let mut idle_polls = 0u32;

        while offset < buf.len() {
            let rintsts = self.regs.rintsts().read();
            if rintsts.error() {
                let err = self.translate_int_error(rintsts, Phase::DataWrite, cmd_index);
                let _ = self.reset_fifo();
                return Err(err);
            }

            let mut pushed_any = false;
            while !self.regs.status().read().fifo_full() && offset < buf.len() {
                let chunk: [u8; 8] = buf[offset..offset + 8].try_into().unwrap();
                let v = u64::from_le_bytes(chunk);
                unsafe { fifo.write_volatile(v) };
                offset += 8;
                pushed_any = true;
            }

            if offset == buf.len() {
                break;
            }

            if pushed_any {
                idle_polls = 0;
            } else {
                idle_polls = idle_polls.saturating_add(1);
                if idle_polls >= POLL_LIMIT {
                    return Err(Error::Timeout(ErrorContext::for_cmd(
                        Phase::DataWrite,
                        cmd_index,
                    )));
                }
                core::hint::spin_loop();
            }
        }

        // Wait until the controller has clocked the last byte out and
        // the card has released busy. DTO is what tells us we're safe
        // to issue the next command.
        self.wait_data_transfer_over(cmd_index, Phase::DataWrite)?;
        // Acknowledge DTO so the next command starts clean.
        let mut ack = crate::regs::RIntSts::new();
        ack.set_data_transfer_over(true);
        self.regs.rintsts().write(ack);
        Ok(())
    }
}
