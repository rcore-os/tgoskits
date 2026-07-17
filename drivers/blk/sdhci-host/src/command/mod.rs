//! Command issue / response collection.
//!
//! Drives the SDHCI command pipeline: argument register → transfer-mode
//! shape (if data is present) → command register → consume acknowledged IRQ
//! snapshots (or initialization-owned status) → harvest the response slot(s).
//!
//! All raise sites tag their phase with [`Phase::CommandSend`] /
//! [`Phase::ResponseWait`] so callers can pinpoint failures.

use sdmmc_protocol::{
    CommandPoll, CommandResponsePoll, DataDirection,
    cmd::Command,
    error::{Error, ErrorContext, Phase},
    response::{IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType},
};

use crate::{
    host::{IrqSnapshot, Sdhci},
    regs::*,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum CommandState {
    Idle,
    WaitingInhibit {
        cmd: Command,
        data: Option<crate::host::PendingData>,
        use_dma: bool,
    },
    WaitingWriteGap {
        cmd: Command,
        has_data: bool,
        command_word: u16,
        wake_at_ns: u64,
    },
    Issued {
        cmd: Command,
    },
    WaitingBusy {
        cmd: Command,
        response: Response,
    },
    Complete {
        response: Response,
    },
    Failed {
        error: Error,
    },
}

impl Sdhci {
    pub fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error> {
        match self.poll_command() {
            Ok(CommandPoll::Pending) => Ok(CommandResponsePoll::Pending),
            Ok(CommandPoll::Complete) => self
                .take_command_response()
                .map(CommandResponsePoll::Complete),
            // Future CommandPoll variants: treat as best-effort harvest, same as Err path.
            Ok(_) => self
                .take_command_response()
                .map(CommandResponsePoll::Complete),
            Err(_) => self
                .take_command_response()
                .map(CommandResponsePoll::Complete),
        }
    }

    /// Program the command register and leave completion to
    /// [`Sdhci::poll_command`].
    pub fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        if !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        self.ensure_command_admissible(cmd, self.pending_data.is_some())?;
        let data = self.pending_data.take();
        info_command_start(self, cmd, data);
        if let Err(error) = self.prepare_irq_for_request() {
            self.pending_data = data;
            return Err(error);
        }
        self.command_state = CommandState::WaitingInhibit {
            cmd: *cmd,
            data,
            use_dma: self.use_dma,
        };
        let low_speed_broadcom_data = self.aligned_32bit_registers()
            && data.is_some()
            && self.bus_clock_hz <= BROADCOM_PACED_MAX_CLOCK_HZ;
        if !low_speed_broadcom_data && let Err(err) = self.poll_command() {
            self.command_state = CommandState::Idle;
            self.clear_cached_irq_status();
            return Err(err);
        }
        Ok(())
    }

    /// Advance the currently submitted command without blocking.
    pub fn poll_command(&mut self) -> Result<CommandPoll, Error> {
        self.poll_command_at_inner(None)
    }

    pub(crate) fn poll_command_at(&mut self, now_ns: u64) -> Result<CommandPoll, Error> {
        self.poll_command_at_inner(Some(now_ns))
    }

    fn poll_command_at_inner(&mut self, now_ns: Option<u64>) -> Result<CommandPoll, Error> {
        match self.command_state {
            CommandState::WaitingInhibit { cmd, data, use_dma } => {
                if !self.command_can_issue(&cmd, data.is_some()) {
                    let err = Error::Busy;
                    self.command_state = CommandState::Failed { error: err };
                    return Err(err);
                }
                self.program_command(&cmd, data, use_dma, now_ns)?;
                return Ok(CommandPoll::Pending);
            }
            CommandState::WaitingWriteGap {
                cmd,
                has_data,
                command_word,
                wake_at_ns,
            } => {
                let Some(now_ns) = now_ns else {
                    let error = Error::UnsupportedCommand;
                    self.command_state = CommandState::Failed { error };
                    return Err(error);
                };
                if now_ns < wake_at_ns {
                    return Ok(CommandPoll::Pending);
                }
                self.issue_command_word(cmd, has_data, command_word);
                return Ok(CommandPoll::Pending);
            }
            CommandState::Issued { .. } => {}
            CommandState::WaitingBusy { cmd, response } => {
                return self.poll_r1b_busy(cmd, response);
            }
            CommandState::Complete { .. } => return Ok(CommandPoll::Complete),
            CommandState::Failed { error, .. } => return Err(error),
            CommandState::Idle => return Err(Error::InvalidArgument),
        }

        let CommandState::Issued { cmd } = self.command_state else {
            unreachable!();
        };

        let snapshot = self.take_command_irq_status();
        if snapshot.has_error() {
            self.log_status("command wait failed", cmd.index);
            let err = self.translate_error_bits(snapshot.error, cmd.index);
            self.command_state = CommandState::Failed { error: err };
            Err(err)
        } else if snapshot.normal & NORMAL_INT_CMD_COMPLETE != 0 {
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
                self.command_state = CommandState::WaitingBusy { cmd, response };
                return self.poll_r1b_busy(cmd, response);
            }
            self.command_state = CommandState::Complete { response };
            Ok(CommandPoll::Complete)
        } else {
            Ok(CommandPoll::Pending)
        }
    }

    pub(crate) fn command_program_wake_at(&self) -> Option<u64> {
        match self.command_state {
            CommandState::WaitingWriteGap { wake_at_ns, .. } => Some(wake_at_ns),
            _ => None,
        }
    }

    fn poll_r1b_busy(&mut self, cmd: Command, response: Response) -> Result<CommandPoll, Error> {
        let snapshot = self.take_busy_irq_status();
        if snapshot.has_error() {
            let err = self.translate_error_bits(snapshot.error, cmd.index);
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        if snapshot.normal & NORMAL_INT_XFER_COMPLETE != 0 {
            self.command_state = CommandState::Complete { response };
            return Ok(CommandPoll::Complete);
        }
        Ok(CommandPoll::Pending)
    }

    fn take_command_irq_status(&mut self) -> IrqSnapshot {
        self.take_irq_snapshot(NORMAL_INT_CMD_COMPLETE | NORMAL_INT_ERROR)
    }

    fn take_busy_irq_status(&mut self) -> IrqSnapshot {
        self.take_irq_snapshot(NORMAL_INT_XFER_COMPLETE | NORMAL_INT_ERROR)
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
            CommandState::WaitingInhibit { .. } | CommandState::WaitingWriteGap { .. } => {
                Err(Error::InvalidArgument)
            }
        }
    }

    pub(crate) fn clear_cached_irq_status(&mut self) {
        self.irq.state.end_request();
        self.pending_irq = IrqSnapshot::empty();
    }

    pub(crate) fn abort_command(&mut self) -> Result<(), Error> {
        if !self.recovery_quiesced {
            return Err(Error::Busy);
        }
        self.clear_cached_irq_status();
        self.pending_data = None;
        self.use_dma = false;
        self.active_data_cmd = 0;
        self.command_state = CommandState::Idle;
        Ok(())
    }

    pub(crate) fn take_data_irq_status(&mut self) -> IrqSnapshot {
        // A DMA-boundary indication is IRQ evidence for the active data
        // request even when the same snapshot also reports transfer complete.
        // Consume it with the data phase so it cannot block the following
        // explicit CMD12 generation handoff.
        self.take_irq_snapshot(
            NORMAL_INT_XFER_COMPLETE | NORMAL_INT_DMA_INTERRUPT | NORMAL_INT_ERROR,
        )
    }

    pub(crate) fn take_fifo_irq_status(&mut self, mask: u16) -> IrqSnapshot {
        self.take_irq_snapshot(mask)
    }

    fn prepare_irq_for_request(&mut self) -> Result<(), Error> {
        if !self.runtime_irq_status_owned() || !self.completion_irq_enabled() {
            return Err(Error::UnsupportedCommand);
        }
        if !self.pending_irq.is_empty() || !self.irq.state.request_handoff_ready() {
            return Err(Error::Busy);
        }
        if !self.irq.state.begin_request() {
            return Err(Error::Busy);
        }
        self.pending_irq = IrqSnapshot::empty();
        Ok(())
    }

    fn take_irq_snapshot(&mut self, normal_mask: u16) -> IrqSnapshot {
        self.collect_irq_snapshot();
        self.pending_irq.take(normal_mask)
    }

    fn collect_irq_snapshot(&mut self) {
        let incoming = self.irq.state.take_snapshot();
        self.pending_irq.merge(incoming);
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
        let (normal, error) = (self.pending_irq.normal, self.pending_irq.error);
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        let power = self.read_u8(REG_POWER_CONTROL);
        let host1 = self.read_u8(REG_HOST_CONTROL1);
        let host2 = self.read_u16(REG_HOST_CONTROL2);
        let reset = self.read_u8(REG_SOFTWARE_RESET);
        let normal_status_enable = self.read_u16(REG_NORMAL_INT_STATUS_ENABLE);
        let error_status_enable = self.read_u16(REG_ERROR_INT_STATUS_ENABLE);
        let normal_signal_enable = self.read_u16(REG_NORMAL_INT_SIGNAL_ENABLE);
        let error_signal_enable = self.read_u16(REG_ERROR_INT_SIGNAL_ENABLE);

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

    pub(crate) fn ensure_command_admissible(
        &self,
        cmd: &Command,
        has_data: bool,
    ) -> Result<(), Error> {
        if !self.runtime_irq_status_owned() || !self.completion_irq_enabled() {
            return Err(Error::UnsupportedCommand);
        }
        if !self.pending_irq.is_empty() || !self.irq.state.request_handoff_ready() {
            return Err(Error::Busy);
        }
        if !self.command_can_issue(cmd, has_data) {
            // An IRQ-owned command must either reach hardware now or remain
            // unaccepted. Re-entering WaitingInhibit from a watchdog/service
            // activation would turn the timer into a completion poller and
            // could issue an accepted request without a matching IRQ cause.
            return Err(Error::Busy);
        }
        Ok(())
    }

    fn program_command(
        &mut self,
        cmd: &Command,
        data: Option<crate::host::PendingData>,
        use_dma: bool,
        now_ns: Option<u64>,
    ) -> Result<(), Error> {
        let has_data = data.is_some();
        let cmd_reg = encode_command(cmd, has_data)?;

        if let Some(d) = data {
            match (use_dma, d.adma_descriptor) {
                (true, Some(descriptor)) => {
                    self.select_adma2_32();
                    self.write_adma_addr(descriptor);
                }
                (false, None) => {}
                _ => return Err(Error::InvalidArgument),
            }
            self.configure_data_phase(d.direction, d.block_size, d.block_count, use_dma);
        } else if use_dma {
            return Err(Error::InvalidArgument);
        } else {
            self.write_u16(REG_TRANSFER_MODE, 0);
        }

        self.write_u32(REG_ARGUMENT, cmd.argument);
        if has_data
            && self.aligned_32bit_registers()
            && self.bus_clock_hz <= BROADCOM_PACED_MAX_CLOCK_HZ
        {
            let now_ns = now_ns.ok_or(Error::UnsupportedCommand)?;
            if !self.flush_aligned_block_shadow() {
                return Err(Error::InvalidArgument);
            }
            self.command_state = CommandState::WaitingWriteGap {
                cmd: *cmd,
                has_data,
                command_word: cmd_reg,
                wake_at_ns: now_ns.saturating_add(broadcom_write_gap_ns(self.bus_clock_hz)),
            };
            return Ok(());
        }

        self.issue_command_word(*cmd, has_data, cmd_reg);
        Ok(())
    }

    fn issue_command_word(&mut self, cmd: Command, has_data: bool, command_word: u16) {
        self.write_u16(REG_COMMAND, command_word);
        if has_data {
            self.active_data_cmd = cmd.index;
        }
        self.log_status("issued", cmd.index);
        self.command_state = CommandState::Issued { cmd };
    }
}

const BROADCOM_PACED_MAX_CLOCK_HZ: u32 = 400_000;

fn broadcom_write_gap_ns(clock_hz: u32) -> u64 {
    if clock_hz == 0 {
        return 10_000;
    }
    4_000_000_000_u64.div_ceil(u64::from(clock_hz))
}

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
mod tests;
