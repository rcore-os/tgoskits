//! Command issue / response collection.
//!
//! Drives the SDHCI command pipeline: argument register → transfer-mode
//! shape (if data is present) → command register → consume IRQ-latched
//! normal/error status → harvest the response slot(s).
//!
//! All raise sites tag their phase with [`Phase::CommandSend`] /
//! [`Phase::ResponseWait`] so callers can pinpoint failures.

use sdmmc_protocol::{
    CommandProgress as CommandPoll, CommandResponseProgress, DataDirection,
    cmd::Command,
    error::{Error, ErrorContext, Phase},
    response::{IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType},
};

use crate::{host::Sdhci, regs::*};

#[derive(Clone, Copy, Debug)]
pub(crate) enum CommandState {
    Idle,
    WaitingInhibit {
        cmd: Command,
        data: Option<crate::host::PendingData>,
        use_dma: bool,
        preserve_irq_generation: bool,
        polls: u32,
    },
    Issued {
        cmd: Command,
        data_line: bool,
        polls: u32,
    },
    WaitingBusy {
        cmd: Command,
        response: Response,
        polls: u32,
    },
    Complete {
        response: Response,
    },
    Failed {
        error: Error,
    },
}

impl Sdhci {
    pub(crate) fn advance_command_response(&mut self) -> Result<CommandResponseProgress, Error> {
        match self.advance_command() {
            Ok(CommandPoll::Pending) => Ok(CommandResponseProgress::Pending),
            Ok(CommandPoll::Complete) => self
                .take_command_response()
                .map(CommandResponseProgress::Complete),
            Err(_) => self
                .take_command_response()
                .map(CommandResponseProgress::Complete),
        }
    }

    /// Program the command register and leave completion to
    /// [`Sdhci::advance_command`].
    pub(crate) fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        self.submit_command_in_generation(cmd, true, None, false)
    }

    pub(crate) fn submit_chained_command(&mut self, cmd: &Command) -> Result<(), Error> {
        self.submit_command_in_generation(cmd, false, None, false)
    }

    pub(crate) fn submit_dma_command(
        &mut self,
        cmd: &Command,
        data: crate::host::PendingData,
    ) -> Result<(), Error> {
        self.submit_command_in_generation(cmd, true, Some(data), true)
    }

    fn submit_command_in_generation(
        &mut self,
        cmd: &Command,
        begin_irq_generation: bool,
        data: Option<crate::host::PendingData>,
        use_dma: bool,
    ) -> Result<(), Error> {
        if !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        info_command_start(self, cmd, data);
        if begin_irq_generation {
            self.prepare_irq_for_request();
        }

        self.command_state = CommandState::WaitingInhibit {
            cmd: *cmd,
            data,
            use_dma,
            preserve_irq_generation: !begin_irq_generation,
            polls: 0,
        };
        if let Err(err) = self.advance_command() {
            self.command_state = CommandState::Idle;
            return Err(err);
        }
        Ok(())
    }

    /// Advance the currently submitted command without blocking.
    pub(crate) fn advance_command(&mut self) -> Result<CommandPoll, Error> {
        match self.command_state {
            CommandState::WaitingInhibit {
                cmd,
                data,
                use_dma,
                preserve_irq_generation,
                polls,
            } => {
                if !self.command_can_issue(&cmd, data.is_some()) {
                    if polls >= COMMAND_WAIT_POLLS {
                        let err =
                            Error::Timeout(ErrorContext::for_cmd(Phase::CommandSend, cmd.index));
                        self.command_state = CommandState::Failed { error: err };
                        return Err(err);
                    }
                    self.command_state = CommandState::WaitingInhibit {
                        cmd,
                        data,
                        use_dma,
                        preserve_irq_generation,
                        polls: polls + 1,
                    };
                    return Ok(CommandPoll::Pending);
                }
                self.program_command(&cmd, data, use_dma, preserve_irq_generation)?;
                return Ok(CommandPoll::Pending);
            }
            CommandState::Issued { .. } => {}
            CommandState::WaitingBusy {
                cmd,
                response,
                polls,
            } => {
                return self.advance_r1b_busy(cmd, response, polls);
            }
            CommandState::Complete { .. } => return Ok(CommandPoll::Complete),
            CommandState::Failed { error, .. } => return Err(error),
            CommandState::Idle => return Err(Error::InvalidArgument),
        }

        let CommandState::Issued {
            cmd,
            data_line,
            polls,
        } = self.command_state
        else {
            unreachable!();
        };

        let (normal, error) = self.take_command_irq_status();
        if normal & NORMAL_INT_CMD_COMPLETE != 0 {
            let response = match decode_response(self, cmd.response) {
                Ok(r) => r,
                Err(err) => {
                    // Park the FSM in Failed before propagating: bare `?` would
                    // leave it in Issued while the IRQ status bits are already
                    // cleared, so the next poll would idle until the caller's
                    // own timeout fires.
                    self.command_state = CommandState::Failed { error: err };
                    return Err(err);
                }
            };
            log::debug!("sdhci: CMD{} response {:?}", cmd.index, response);
            if matches!(cmd.response, ResponseType::R1b) {
                return self.advance_r1b_busy(cmd, response, 0);
            }
            self.command_state = CommandState::Complete { response };
            Ok(CommandPoll::Complete)
        } else if normal & NORMAL_INT_ERROR != 0 {
            self.log_status("command wait failed", cmd.index);
            self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
            let _ = self.reset_cmd();
            if data_line {
                let _ = self.reset_dat();
            }
            let err = self.translate_error_bits(error & ERROR_INT_CMD_LINE_MASK, cmd.index);
            self.command_state = CommandState::Failed { error: err };
            Err(err)
        } else {
            if polls >= COMMAND_WAIT_POLLS {
                self.log_status("command response timeout", cmd.index);
                let _ = self.reset_cmd();
                if data_line {
                    let _ = self.reset_dat();
                }
                let err = Error::Timeout(ErrorContext::for_cmd(Phase::ResponseWait, cmd.index));
                self.command_state = CommandState::Failed { error: err };
                return Err(err);
            }
            self.command_state = CommandState::Issued {
                cmd,
                data_line,
                polls: polls + 1,
            };
            Ok(CommandPoll::Pending)
        }
    }

    fn advance_r1b_busy(
        &mut self,
        cmd: Command,
        response: Response,
        polls: u32,
    ) -> Result<CommandPoll, Error> {
        let (normal, error) = self.take_command_irq_status();
        if normal & NORMAL_INT_ERROR != 0 {
            self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
            let _ = self.reset_cmd();
            let _ = self.reset_dat();
            let err = self.translate_error_bits(error & ERROR_INT_DATA_LINE_MASK, cmd.index);
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        let present = self.read_u32(REG_PRESENT_STATE);
        if present & PRESENT_DAT0_LINE_SIGNAL_LEVEL != 0 {
            self.command_state = CommandState::Complete { response };
            return Ok(CommandPoll::Complete);
        }
        if polls >= COMMAND_BUSY_POLLS {
            let _ = self.reset_dat();
            let err = Error::Timeout(ErrorContext::for_cmd(Phase::BusyWait, cmd.index));
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        self.command_state = CommandState::WaitingBusy {
            cmd,
            response,
            polls: polls + 1,
        };
        Ok(CommandPoll::Pending)
    }

    fn take_command_irq_status(&mut self) -> (u16, u16) {
        let normal = self
            .irq
            .state
            .take_normal(NORMAL_INT_CMD_COMPLETE | NORMAL_INT_ERROR);
        let error = self.irq.state.take_error_all();
        if error != 0 {
            self.irq.state.clear_normal(NORMAL_INT_ERROR);
        }
        (normal, error)
    }

    /// Reports whether the active command can advance from register state
    /// alone after its preceding IRQ has been consumed.
    pub(crate) fn progress_wait_kind(&self) -> sdmmc_protocol::sdio::host::HostProgressWait {
        use sdmmc_protocol::sdio::host::HostProgressWait;

        if self.command_needs_register_retry() {
            HostProgressWait::Register {
                retry_after: crate::SDHCI_REGISTER_RETRY_DELAY,
            }
        } else {
            HostProgressWait::Irq
        }
    }

    pub(crate) fn command_needs_register_retry(&self) -> bool {
        matches!(
            self.command_state,
            CommandState::WaitingInhibit { .. } | CommandState::WaitingBusy { .. }
        )
    }

    pub fn take_command_response(&mut self) -> Result<Response, Error> {
        match self.command_state {
            CommandState::Complete { response, .. } => {
                self.command_state = CommandState::Idle;
                if self.active_data_cmd == 0 {
                    self.clear_cached_irq_status();
                }
                Ok(response)
            }
            CommandState::Failed { error, .. } => {
                self.command_state = CommandState::Idle;
                self.clear_cached_irq_status();
                Err(error)
            }
            CommandState::Idle | CommandState::Issued { .. } | CommandState::WaitingBusy { .. } => {
                Err(Error::InvalidArgument)
            }
            CommandState::WaitingInhibit { .. } => Err(Error::InvalidArgument),
        }
    }

    pub(crate) fn clear_cached_irq_status(&mut self) {
        self.irq.state.end_request();
    }

    pub(crate) fn abort_command(&mut self) -> Result<(), Error> {
        self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
        self.clear_cached_irq_status();
        self.reset_cmd()?;
        self.reset_dat()?;
        self.active_data_cmd = 0;
        self.command_state = CommandState::Idle;
        Ok(())
    }

    pub(crate) fn take_data_irq_status(&mut self) -> (u16, u16) {
        let normal = self
            .irq
            .state
            .take_normal(NORMAL_INT_XFER_COMPLETE | NORMAL_INT_ERROR);
        let error = self.irq.state.take_error_all();
        if error != 0 {
            self.irq.state.clear_normal(NORMAL_INT_ERROR);
        }
        (normal, error)
    }

    fn prepare_irq_for_request(&mut self) {
        self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
        self.irq.state.begin_request();
    }

    fn translate_error_bits(&self, err: u16, cmd_index: u8) -> Error {
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
        let (normal, error) = self.read_interrupt_status();
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        let power = self.read_u8(REG_POWER_CONTROL);
        let host1 = self.read_u8(REG_HOST_CONTROL1);
        let host2 = self.read_u16(REG_HOST_CONTROL2);
        let reset = self.read_u8(REG_SOFTWARE_RESET);
        let (normal_status_enable, error_status_enable) = self.read_interrupt_status_enable();
        let (normal_signal_enable, error_signal_enable) = self.read_interrupt_signal_enable();

        if reason == "issued" {
            log::debug!(
                "sdhci: {} CMD{} present={:#010x} normal={:#06x} error={:#06x} clock={:#06x} \
                 power={:#04x} host1={:#04x} host2={:#06x} reset={:#04x} nisen={:#06x} \
                 eisen={:#06x} nsigen={:#06x} esigen={:#06x}",
                reason,
                cmd_index,
                present,
                normal,
                error,
                clock,
                power,
                host1,
                host2,
                reset,
                normal_status_enable,
                error_status_enable,
                normal_signal_enable,
                error_signal_enable
            );
        } else {
            log::info!(
                "sdhci: {} CMD{} present={:#010x} normal={:#06x} error={:#06x} clock={:#06x} \
                 power={:#04x} host1={:#04x} host2={:#06x} reset={:#04x} nisen={:#06x} \
                 eisen={:#06x} nsigen={:#06x} esigen={:#06x}",
                reason,
                cmd_index,
                present,
                normal,
                error,
                clock,
                power,
                host1,
                host2,
                reset,
                normal_status_enable,
                error_status_enable,
                normal_signal_enable,
                error_signal_enable
            );
        }
    }

    fn configure_data_phase(
        &mut self,
        direction: DataDirection,
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

    fn command_can_issue(&self, cmd: &Command, has_data: bool) -> bool {
        self.read_u32(REG_PRESENT_STATE) & command_inhibit_mask(cmd, has_data) == 0
    }

    fn program_command(
        &mut self,
        cmd: &Command,
        data: Option<crate::host::PendingData>,
        use_dma: bool,
        preserve_irq_generation: bool,
    ) -> Result<(), Error> {
        let has_data = data.is_some();
        let data_line = command_uses_data_line(cmd, has_data);

        if !preserve_irq_generation {
            self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
            // Keep the active request generation alive while dropping status
            // from the previous request.
            self.irq.state.clear_all();
        }

        if let Some(d) = data {
            self.configure_data_phase(d.direction, d.block_size, d.block_count, use_dma);
        } else {
            self.write_u16(REG_TRANSFER_MODE, 0);
        }

        self.write_u32(REG_ARGUMENT, cmd.argument);
        let cmd_reg = encode_command(cmd, has_data)?;
        self.write_u16(REG_COMMAND, cmd_reg);
        if has_data {
            self.active_data_cmd = cmd.index;
        }
        self.log_status("issued", cmd.index);
        self.command_state = CommandState::Issued {
            cmd: *cmd,
            data_line,
            polls: 0,
        };
        Ok(())
    }
}

const COMMAND_WAIT_POLLS: u32 = 1_000_000;
const COMMAND_BUSY_POLLS: u32 = 1_000_000;

fn transfer_mode(direction: DataDirection, block_count: u32, use_dma: bool) -> u16 {
    let mut mode = XFER_MODE_BLOCK_COUNT_ENABLE;
    if block_count > 1 {
        mode |= XFER_MODE_MULTI_BLOCK;
    }
    if matches!(direction, DataDirection::Read) {
        mode |= XFER_MODE_READ;
    }
    if use_dma {
        mode |= XFER_MODE_DMA_ENABLE;
    }
    mode
}

fn command_inhibit_mask(cmd: &Command, has_data: bool) -> u32 {
    let mut mask = PRESENT_CMD_INHIBIT;
    if command_uses_data_line(cmd, has_data) {
        mask |= PRESENT_DAT_INHIBIT;
    }
    if cmd.index == sdmmc_protocol::cmd::CMD12.index {
        mask &= !PRESENT_DAT_INHIBIT;
    }
    mask
}

fn command_uses_data_line(cmd: &Command, has_data: bool) -> bool {
    has_data || matches!(cmd.response, ResponseType::R1b)
}

fn info_command_start(host: &Sdhci, cmd: &Command, data: Option<crate::host::PendingData>) {
    match data {
        Some(data) => log::debug!(
            "sdhci: CMD{} arg={:#010x} resp={:?} data={:?} blocks={} block_size={} \
             present={:#010x}",
            cmd.index,
            cmd.argument,
            cmd.response,
            data.direction,
            data.block_count,
            data.block_size,
            host.read_u32(REG_PRESENT_STATE)
        ),
        None => log::debug!(
            "sdhci: CMD{} arg={:#010x} resp={:?} data=none present={:#010x}",
            cmd.index,
            cmd.argument,
            cmd.response,
            host.read_u32(REG_PRESENT_STATE)
        ),
    }
}

fn encode_command(cmd: &Command, has_data: bool) -> Result<u16, Error> {
    let resp_bits: u16 = match cmd.response {
        ResponseType::None => CMD_RESP_NONE,
        ResponseType::R1 | ResponseType::R5 | ResponseType::R6 | ResponseType::R7 => {
            CMD_RESP_LEN48 | CMD_CRC_CHECK | CMD_INDEX_CHECK
        }
        ResponseType::R1b => CMD_RESP_LEN48_BUSY | CMD_CRC_CHECK | CMD_INDEX_CHECK,
        ResponseType::R2 => CMD_RESP_LEN136 | CMD_CRC_CHECK,
        ResponseType::R3 | ResponseType::R4 => CMD_RESP_LEN48,
        // Future ResponseType variants are unsupported by this encoder.
        _ => return Err(Error::UnsupportedCommand),
    };

    let data_bit = if has_data { CMD_DATA_PRESENT } else { 0 };
    let cmd_index = (cmd.index as u16) << 8;
    Ok(cmd_index | data_bit | resp_bits)
}

fn decode_response(host: &Sdhci, resp_type: ResponseType) -> Result<Response, Error> {
    Ok(match resp_type {
        ResponseType::None => Response::Empty,
        ResponseType::R1 => Response::R1(R1Response {
            raw: host.response32(0),
        }),
        ResponseType::R1b => Response::R1b(R1Response {
            raw: host.response32(0),
        }),
        ResponseType::R2 => Response::R2(read_r2(host)),
        ResponseType::R3 => Response::R3(OcrResponse::from_raw(host.response32(0))),
        ResponseType::R4 => Response::R4(sdmmc_protocol::response::SdioOcrResponse::from_raw(
            host.response32(0),
        )),
        ResponseType::R5 => Response::R5(sdmmc_protocol::response::SdioRwResponse::from_raw(
            host.response32(0),
        )),
        ResponseType::R6 => Response::R6(RcaResponse::from_raw(host.response32(0))),
        ResponseType::R7 => Response::R7(IfCondResponse::from_raw(host.response32(0))),
        // Future ResponseType variants are not decoded by this controller.
        _ => return Err(Error::UnsupportedCommand),
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
#[path = "command_tests.rs"]
mod tests;
