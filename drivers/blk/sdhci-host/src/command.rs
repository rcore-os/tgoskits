//! Command issue / response collection.
//!
//! Drives the SDHCI command pipeline: argument register → transfer-mode
//! shape (if data is present) → command register → poll the normal/error
//! interrupt status registers → harvest the response slot(s).
//!
//! All raise sites tag their phase with [`Phase::CommandSend`] /
//! [`Phase::ResponseWait`] so callers can pinpoint failures.

use sdmmc_protocol::{
    cmd::Command,
    error::{Error, ErrorContext, Phase},
    response::{IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType},
};

use crate::{host::Sdhci, regs::*};

const POLL_LIMIT: u32 = 1_000_000;

impl Sdhci {
    /// Issue a single command. Caller must have populated `pending_data`
    /// (the `SdioHost::prepare_data_transfer` impl does this) when the
    /// command carries a data phase.
    pub fn issue_command(&mut self, cmd: &Command) -> Result<Response, Error> {
        let data = self.pending_data.take();
        let has_data = data.is_some();
        info_command_start(self, cmd, data);

        // 1. Wait for the controller's own pipeline to drain.
        self.wait_inhibit(has_data, cmd.cmd)?;

        // 2. Clear any leftover interrupt bits.
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);

        // 3. Configure the data phase (block size + count + transfer mode).
        if let Some(d) = data {
            self.configure_data_phase(d.direction, d.block_size, d.block_count, self.use_dma);
        } else {
            self.write_u16(REG_TRANSFER_MODE, 0);
        }

        // 4. Push the argument and command-register encoding.
        self.write_u32(REG_ARGUMENT, cmd.arg);
        let cmd_reg = encode_command(cmd, has_data)?;
        self.write_u16(REG_COMMAND, cmd_reg);
        if has_data {
            self.active_data_cmd = cmd.cmd;
        }
        self.log_status("issued", cmd.cmd);

        // 5. Block until the response arrives (or the controller flags
        //    a CMD-line error).
        if let Err(err) = self.wait_for(NORMAL_INT_CMD_COMPLETE, ERROR_INT_CMD_LINE_MASK, cmd.cmd) {
            self.log_status("command wait failed", cmd.cmd);
            self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
            self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
            let _ = self.reset_cmd();
            if has_data {
                let _ = self.reset_dat();
            }
            return Err(err);
        }

        // 6. Acknowledge command completion.
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CMD_COMPLETE);

        let response = decode_response(self, cmd.resp_type)?;
        log::debug!("sdhci: CMD{} response {:?}", cmd.cmd, response);
        Ok(response)
    }

    /// Block until the next data phase finishes (Transfer Complete) or
    /// the controller raises a DAT-line error.
    pub fn wait_data_complete(&mut self, cmd_index: u8) -> Result<(), Error> {
        if let Err(err) = self.wait_for(
            NORMAL_INT_XFER_COMPLETE,
            ERROR_INT_DATA_LINE_MASK,
            cmd_index,
        ) {
            self.log_status("data wait failed", cmd_index);
            self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
            self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
            let _ = self.reset_cmd();
            let _ = self.reset_dat();
            return Err(err);
        }
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_XFER_COMPLETE);
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
        Ok(())
    }

    /// Same as [`wait_data_complete`] but also surfaces ADMA-engine
    /// errors. Used by the DMA data path.
    pub fn wait_data_complete_with_adma(&self, cmd_index: u8, phase: Phase) -> Result<(), Error> {
        for _ in 0..POLL_LIMIT {
            let status = self.read_u16(REG_NORMAL_INT_STATUS);
            if status & NORMAL_INT_XFER_COMPLETE != 0 {
                self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_XFER_COMPLETE);
                return Ok(());
            }
            if status & NORMAL_INT_ERROR != 0 {
                let err = self.read_u16(REG_ERROR_INT_STATUS);
                self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
                let ctx = ErrorContext::for_cmd(phase, cmd_index);
                return Err(if err & ERROR_INT_ADMA != 0 {
                    // ADMA engine raised an error — treat as misaligned
                    // descriptor / address overrun.
                    Error::Misaligned
                } else if err & (ERROR_INT_DATA_TIMEOUT | ERROR_INT_CMD_TIMEOUT) != 0 {
                    Error::Timeout(ctx)
                } else if err & (ERROR_INT_DATA_CRC | ERROR_INT_CMD_CRC) != 0 {
                    Error::Crc(ctx)
                } else if matches!(phase, Phase::DataRead) {
                    Error::ReadError(ctx)
                } else {
                    Error::WriteError(ctx)
                });
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::for_cmd(phase, cmd_index)))
    }

    fn wait_inhibit(&self, has_data: bool, cmd_index: u8) -> Result<(), Error> {
        let mask = if has_data {
            PRESENT_CMD_INHIBIT | PRESENT_DAT_INHIBIT
        } else {
            PRESENT_CMD_INHIBIT
        };
        for _ in 0..POLL_LIMIT {
            if self.read_u32(REG_PRESENT_STATE) & mask == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        self.log_status("inhibit wait timed out", cmd_index);
        Err(Error::Timeout(ErrorContext::for_cmd(
            Phase::CommandSend,
            cmd_index,
        )))
    }

    fn wait_for(&self, success_mask: u16, error_mask: u16, cmd_index: u8) -> Result<(), Error> {
        for _ in 0..POLL_LIMIT {
            let status = self.read_u16(REG_NORMAL_INT_STATUS);
            if status & success_mask != 0 {
                return Ok(());
            }
            if status & NORMAL_INT_ERROR != 0 {
                self.log_status("interrupt error", cmd_index);
                return Err(self.translate_error(error_mask, cmd_index));
            }
            core::hint::spin_loop();
        }
        self.log_status("interrupt wait timed out", cmd_index);
        Err(Error::Timeout(ErrorContext::for_cmd(
            Phase::ResponseWait,
            cmd_index,
        )))
    }

    fn translate_error(&self, mask: u16, cmd_index: u8) -> Error {
        let err = self.read_u16(REG_ERROR_INT_STATUS) & mask;
        // Acknowledge so the next command starts from a clean slate.
        self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
        let ctx = ErrorContext::for_cmd(Phase::ResponseWait, cmd_index);
        if err & (ERROR_INT_CMD_TIMEOUT | ERROR_INT_DATA_TIMEOUT) != 0 {
            Error::Timeout(ctx)
        } else if err & (ERROR_INT_CMD_CRC | ERROR_INT_DATA_CRC) != 0 {
            Error::Crc(ctx)
        } else if err & ERROR_INT_DATA_LINE_MASK != 0 {
            Error::ReadError(ctx)
        } else {
            Error::BadResponse(ctx)
        }
    }

    pub(crate) fn log_status(&self, reason: &str, cmd_index: u8) {
        let present = self.read_u32(REG_PRESENT_STATE);
        let normal = self.read_u16(REG_NORMAL_INT_STATUS);
        let error = self.read_u16(REG_ERROR_INT_STATUS);
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        let power = self.read_u8(REG_POWER_CONTROL);
        let host1 = self.read_u8(REG_HOST_CONTROL1);
        let host2 = self.read_u16(REG_HOST_CONTROL2);
        let reset = self.read_u8(REG_SOFTWARE_RESET);

        if reason == "issued" {
            log::debug!(
                "sdhci: {} CMD{} present={:#010x} normal={:#06x} error={:#06x} clock={:#06x} \
                 power={:#04x} host1={:#04x} host2={:#06x} reset={:#04x}",
                reason,
                cmd_index,
                present,
                normal,
                error,
                clock,
                power,
                host1,
                host2,
                reset
            );
        } else {
            log::info!(
                "sdhci: {} CMD{} present={:#010x} normal={:#06x} error={:#06x} clock={:#06x} \
                 power={:#04x} host1={:#04x} host2={:#06x} reset={:#04x}",
                reason,
                cmd_index,
                present,
                normal,
                error,
                clock,
                power,
                host1,
                host2,
                reset
            );
        }
    }

    fn configure_data_phase(
        &mut self,
        direction: sdmmc_protocol::DataDirection,
        block_size: u32,
        block_count: u32,
        use_dma: bool,
    ) {
        // SDHCI block size register: bits 11..0 hold block length, bits
        // 14..12 hold the SDMA buffer boundary (we use 0 = 4 KiB).
        self.write_u16(REG_BLOCK_SIZE, (block_size as u16) & 0x0FFF);
        self.write_u16(REG_BLOCK_COUNT, block_count as u16);

        let mode = transfer_mode(direction, block_count, use_dma);
        self.write_u8(REG_TIMEOUT_CONTROL, 0x0E);
        self.write_u16(REG_TRANSFER_MODE, mode);
    }
}

fn transfer_mode(direction: sdmmc_protocol::DataDirection, block_count: u32, use_dma: bool) -> u16 {
    let mut mode = XFER_MODE_BLOCK_COUNT_ENABLE;
    if block_count > 1 {
        mode |= XFER_MODE_MULTI_BLOCK;
    }
    if matches!(direction, sdmmc_protocol::DataDirection::Read) {
        mode |= XFER_MODE_READ;
    }
    if use_dma {
        mode |= XFER_MODE_DMA_ENABLE;
    }
    mode
}

fn info_command_start(host: &Sdhci, cmd: &Command, data: Option<crate::host::PendingData>) {
    match data {
        Some(data) => log::debug!(
            "sdhci: CMD{} arg={:#010x} resp={:?} data={:?} blocks={} block_size={} \
             present={:#010x}",
            cmd.cmd,
            cmd.arg,
            cmd.resp_type,
            data.direction,
            data.block_count,
            data.block_size,
            host.read_u32(REG_PRESENT_STATE)
        ),
        None => log::debug!(
            "sdhci: CMD{} arg={:#010x} resp={:?} data=none present={:#010x}",
            cmd.cmd,
            cmd.arg,
            cmd.resp_type,
            host.read_u32(REG_PRESENT_STATE)
        ),
    }
}

fn encode_command(cmd: &Command, has_data: bool) -> Result<u16, Error> {
    let resp_bits: u16 = match cmd.resp_type {
        ResponseType::None => CMD_RESP_NONE,
        ResponseType::R1 | ResponseType::R5 | ResponseType::R6 | ResponseType::R7 => {
            CMD_RESP_LEN48 | CMD_CRC_CHECK | CMD_INDEX_CHECK
        }
        ResponseType::R1b => CMD_RESP_LEN48_BUSY | CMD_CRC_CHECK | CMD_INDEX_CHECK,
        ResponseType::R2 => CMD_RESP_LEN136 | CMD_CRC_CHECK,
        ResponseType::R3 | ResponseType::R4 => CMD_RESP_LEN48,
    };

    let data_bit = if has_data { CMD_DATA_PRESENT } else { 0 };
    let cmd_index = (cmd.cmd as u16) << 8;
    Ok(cmd_index | data_bit | resp_bits)
}

fn decode_response(host: &Sdhci, resp_type: ResponseType) -> Result<Response, Error> {
    Ok(match resp_type {
        ResponseType::None => Response::None,
        ResponseType::R1 | ResponseType::R1b => Response::R1(R1Response {
            raw: host.response32(0),
        }),
        ResponseType::R2 => Response::R2(read_r2(host)),
        ResponseType::R3 => Response::R3(OcrResponse::from_raw(host.response32(0))),
        ResponseType::R4 | ResponseType::R5 => {
            // SDIO IO commands aren't part of the MVP; surface them as
            // "bad response" rather than silently returning zeros.
            return Err(Error::BadResponse(ErrorContext::default()));
        }
        ResponseType::R6 => Response::R6(RcaResponse::from_raw(host.response32(0))),
        ResponseType::R7 => Response::R7(IfCondResponse::from_raw(host.response32(0))),
    })
}

/// Reconstruct the on-bus 128-bit R2 frame from the four 32-bit response
/// slots, then serialize it MSB-first into the 16-byte buffer that the
/// protocol layer's [`sdmmc_protocol::CsdResponse`] / `CidResponse`
/// parsers expect.
///
/// SDHCI strips the start/tr/reserved header (top 8 bits of the on-bus
/// frame) and the CRC7+end (bottom 8 bits), then stores `card_resp[127:8]`
/// shifted up by 8 across `REG_RESPONSE0..REG_RESPONSE3`. We undo the
/// shift the same way Linux's `sdhci_finish_command` does.
fn read_r2(host: &Sdhci) -> [u8; 16] {
    let raw0 = host.response32(0);
    let raw1 = host.response32(1);
    let raw2 = host.response32(2);
    let raw3 = host.response32(3);

    let words = [
        (raw3 << 8) | (raw2 >> 24),
        (raw2 << 8) | (raw1 >> 24),
        (raw1 << 8) | (raw0 >> 24),
        raw0 << 8,
    ];

    let mut bytes = [0u8; 16];
    for (i, word) in words.iter().enumerate() {
        bytes[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use sdmmc_protocol::DataDirection;

    use super::*;

    #[test]
    fn multi_block_transfer_mode_leaves_stop_command_to_protocol_layer() {
        let mode = transfer_mode(DataDirection::Read, 4, false);

        assert_ne!(mode & XFER_MODE_MULTI_BLOCK, 0);
        assert_eq!(mode & XFER_MODE_AUTO_CMD12, 0);
    }
}
