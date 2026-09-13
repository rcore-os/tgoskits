use sdmmc_protocol::{
    CommandProgress, CommandResponseProgress,
    cmd::{Command as ProtoCmd, DataDirection},
    error::{Error, ErrorContext, Phase},
    response::{
        IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType,
        SdioOcrResponse, SdioRwResponse,
    },
};

use crate::{
    host::PhytiumMci,
    regs::{Cmd, RIntSts, RegisterBlockVolatileFieldAccess},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum CommandState {
    Idle,
    WaitingInhibit {
        cmd: ProtoCmd,
        data: Option<crate::host::PendingData>,
        polls: u32,
    },
    WaitingStart {
        cmd: ProtoCmd,
        polls: u32,
    },
    Issued {
        cmd: ProtoCmd,
        polls: u32,
    },
    WaitingBusy {
        cmd: ProtoCmd,
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

impl PhytiumMci {
    pub(crate) fn advance_command_response(
        &mut self,
        cause: sdmmc_host::ProgressCause,
    ) -> Result<CommandResponseProgress, Error> {
        let acknowledged_irq = cause == sdmmc_host::ProgressCause::AcknowledgedIrq;
        match self.advance_command_for_cause(acknowledged_irq) {
            Ok(CommandProgress::Pending) => Ok(CommandResponseProgress::Pending),
            Ok(CommandProgress::Complete) => self
                .take_command_response()
                .map(CommandResponseProgress::Complete),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn submit_command(&mut self, cmd: &ProtoCmd) -> Result<(), Error> {
        self.submit_command_in_generation(cmd, true)
    }

    pub(crate) fn submit_chained_command(&mut self, cmd: &ProtoCmd) -> Result<(), Error> {
        self.submit_command_in_generation(cmd, false)
    }

    fn submit_command_in_generation(
        &mut self,
        cmd: &ProtoCmd,
        begin_irq_generation: bool,
    ) -> Result<(), Error> {
        if !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        let data = self.pending_data.take();
        if data.is_none() && begin_irq_generation {
            self.prepare_irq_for_request();
        }
        self.command_state = CommandState::WaitingInhibit {
            cmd: *cmd,
            data,
            polls: 0,
        };
        if let Err(err) = self.advance_command() {
            self.command_state = CommandState::Idle;
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn advance_command_for_cause(
        &mut self,
        acknowledged_irq: bool,
    ) -> Result<CommandProgress, Error> {
        let was_waiting_for_start = matches!(self.command_state, CommandState::WaitingStart { .. });
        let progress = self.advance_command()?;
        if acknowledged_irq
            && was_waiting_for_start
            && matches!(progress, CommandProgress::Pending)
            && matches!(self.command_state, CommandState::Issued { .. })
        {
            // A fast command may complete before the maintenance thread's
            // register retry observes START_CMD clearing. The IRQ event is
            // already latched, so consume it in the same acknowledged-IRQ
            // transition instead of sleeping for an interrupt that has
            // already happened.
            return self.advance_command();
        }
        Ok(progress)
    }

    pub(crate) fn advance_command(&mut self) -> Result<CommandProgress, Error> {
        match self.command_state {
            CommandState::WaitingInhibit { cmd, data, polls } => {
                if !self.command_can_issue(data.is_some()) {
                    if polls >= COMMAND_WAIT_POLLS {
                        let err =
                            Error::Timeout(ErrorContext::for_cmd(Phase::CommandSend, cmd.index));
                        self.command_state = CommandState::Failed { error: err };
                        return Err(err);
                    }
                    self.command_state = CommandState::WaitingInhibit {
                        cmd,
                        data,
                        polls: polls + 1,
                    };
                    return Ok(CommandProgress::Pending);
                }
                self.program_command(&cmd, data);
                return Ok(CommandProgress::Pending);
            }
            CommandState::WaitingStart { cmd, polls } => {
                if self.regs.cmd().read().start_cmd() {
                    if polls >= COMMAND_WAIT_POLLS {
                        let err =
                            Error::Timeout(ErrorContext::for_cmd(Phase::CommandSend, cmd.index));
                        self.command_state = CommandState::Failed { error: err };
                        return Err(err);
                    }
                    self.command_state = CommandState::WaitingStart {
                        cmd,
                        polls: polls + 1,
                    };
                    return Ok(CommandProgress::Pending);
                }
                self.command_state = CommandState::Issued { cmd, polls: 0 };
                return Ok(CommandProgress::Pending);
            }
            CommandState::Issued { .. } => {}
            CommandState::WaitingBusy {
                cmd,
                response,
                polls,
            } => return self.advance_r1b_busy(cmd, response, polls),
            CommandState::Complete { .. } => return Ok(CommandProgress::Complete),
            CommandState::Failed { error } => return Err(error),
            CommandState::Idle => return Err(Error::InvalidArgument),
        }

        let CommandState::Issued { cmd, polls } = self.command_state else {
            unreachable!();
        };
        let raw_idsts = self
            .irq
            .state
            .take_idmac_status(crate::MCI_IDSTS_LATCH_ERROR_MASK);
        if raw_idsts != 0 {
            let phase = if cmd.index == 12 {
                Phase::BusyWait
            } else {
                Phase::ResponseWait
            };
            let err = Error::BusError(ErrorContext::for_cmd(phase, cmd.index));
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        let raw_status = self.take_command_irq_status();
        let status = RIntSts::from_bits(raw_status);
        if status.error() {
            let err = self.translate_int_error(status, Phase::ResponseWait, cmd.index);
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        if status.command_done() {
            let response = match decode_response(self, cmd.response) {
                Ok(r) => r,
                Err(err) => {
                    // Park the FSM in Failed before propagating so the next
                    // `take_command_response` sees the diagnostic instead of
                    // re-entering Issued and re-reading already-cleared IRQ
                    // status.
                    self.command_state = CommandState::Failed { error: err };
                    return Err(err);
                }
            };
            if matches!(cmd.response, ResponseType::R1b) {
                return self.advance_r1b_busy(cmd, response, 0);
            }
            self.command_state = CommandState::Complete { response };
            return Ok(CommandProgress::Complete);
        }
        if polls >= COMMAND_WAIT_POLLS {
            let err = Error::Timeout(ErrorContext::for_cmd(Phase::ResponseWait, cmd.index));
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        self.command_state = CommandState::Issued {
            cmd,
            polls: polls + 1,
        };
        Ok(CommandProgress::Pending)
    }

    fn advance_r1b_busy(
        &mut self,
        cmd: ProtoCmd,
        response: Response,
        polls: u32,
    ) -> Result<CommandProgress, Error> {
        let raw_idsts = self
            .irq
            .state
            .take_idmac_status(crate::MCI_IDSTS_LATCH_ERROR_MASK);
        if raw_idsts != 0 {
            let err = Error::BusError(ErrorContext::for_cmd(Phase::BusyWait, cmd.index));
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        let raw_status = self.take_command_irq_status();
        let status = RIntSts::from_bits(raw_status);
        if status.error() {
            let err = self.translate_int_error(status, Phase::BusyWait, cmd.index);
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        if !self.regs.status().read().data_busy() {
            self.command_state = CommandState::Complete { response };
            return Ok(CommandProgress::Complete);
        }
        if polls >= COMMAND_BUSY_POLLS {
            let err = Error::Timeout(ErrorContext::for_cmd(Phase::BusyWait, cmd.index));
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        self.command_state = CommandState::WaitingBusy {
            cmd,
            response,
            polls: polls + 1,
        };
        Ok(CommandProgress::Pending)
    }

    pub fn take_command_response(&mut self) -> Result<Response, Error> {
        match self.command_state {
            CommandState::Complete { response } => {
                self.command_state = CommandState::Idle;
                if self.data_cmd_index == 0 {
                    self.irq.state.end_request();
                }
                Ok(response)
            }
            CommandState::Failed { error } => {
                self.command_state = CommandState::Idle;
                self.irq.state.end_request();
                Err(error)
            }
            CommandState::Idle
            | CommandState::WaitingInhibit { .. }
            | CommandState::WaitingStart { .. }
            | CommandState::Issued { .. }
            | CommandState::WaitingBusy { .. } => Err(Error::InvalidArgument),
        }
    }

    fn command_can_issue(&self, has_data: bool) -> bool {
        let cmd_busy = self.regs.cmd().read().start_cmd();
        let data_busy = has_data && self.regs.status().read().data_busy();
        !cmd_busy && !data_busy
    }

    fn program_command(&mut self, cmd: &ProtoCmd, data: Option<crate::host::PendingData>) {
        if data.is_some() {
            self.data_cmd_index = cmd.index;
        }
        let data_dir = data.map(|d| {
            self.program_data_phase(d.block_size, d.block_count);
            d.direction
        });
        self.regs.cmdarg().write(cmd.argument);
        let encoded = encode_command(cmd, data_dir).with_use_hold_reg(self.use_hold_reg);
        self.regs.cmd().write(encoded);
        self.command_state = CommandState::WaitingStart {
            cmd: *cmd,
            polls: 0,
        };
    }

    fn take_command_irq_status(&mut self) -> u32 {
        self.irq
            .state
            .take_status(crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_ERROR_MASK)
    }

    fn clear_command_int_status(&mut self) {
        let raw_status = self.regs.rintsts().read().into_bits()
            & (crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_ERROR_MASK);
        if raw_status != 0 {
            self.regs.rintsts().write(RIntSts::from_bits(raw_status));
        }
        self.irq
            .state
            .clear_status(crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_ERROR_MASK);
    }

    fn prepare_irq_for_request(&mut self) {
        self.clear_all_int_status();
        self.regs.idsts().write(u32::MAX);
        self.irq.state.begin_request();
    }

    pub(crate) fn abort_command(&mut self) -> Result<(), Error> {
        self.clear_command_int_status();
        for _ in 0..COMMAND_WAIT_POLLS {
            if !self.regs.cmd().read().start_cmd() {
                self.clear_all_int_status();
                self.reset_fifo(Phase::CommandSend)?;
                self.reset_dma(Phase::CommandSend)?;
                self.pending_data = None;
                self.data_blocks_remaining = 0;
                self.data_cmd_index = 0;
                self.command_state = CommandState::Idle;
                return Ok(());
            }
            core::hint::spin_loop();
        }
        self.reset_and_init_preserving_irq()?;
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.command_state = CommandState::Idle;
        Ok(())
    }
}

const COMMAND_WAIT_POLLS: u32 = 1_000_000;
const COMMAND_BUSY_POLLS: u32 = 1_000_000;

pub(crate) fn encode_command(cmd: &ProtoCmd, data_dir: Option<DataDirection>) -> Cmd {
    let mut c = Cmd::new()
        .with_start_cmd(true)
        .with_use_hold_reg(true)
        .with_wait_prvdata_complete(true)
        .with_cmd_index(cmd.index & 0x3F);

    match cmd.response {
        ResponseType::None => {}
        ResponseType::R1 | ResponseType::R5 | ResponseType::R6 | ResponseType::R7 => {
            c = c.with_response_expect(true).with_check_response_crc(true);
        }
        ResponseType::R1b => {
            c = c.with_response_expect(true).with_check_response_crc(true);
        }
        ResponseType::R2 => {
            c = c
                .with_response_expect(true)
                .with_response_length(true)
                .with_check_response_crc(true);
        }
        ResponseType::R3 | ResponseType::R4 => {
            c = c.with_response_expect(true);
        }
        // Future ResponseType variants land here as bare command; controller default is no response_expect.
        _ => {}
    }

    if cmd.index == 0 {
        c = c.with_send_initialization(true);
    }
    if cmd.index == 12 {
        c = c.with_stop_abort_cmd(true);
    }

    if let Some(dir) = data_dir {
        c = c.with_data_expected(true);
        if matches!(dir, DataDirection::Write) {
            c = c.with_read_write(true);
        }
    }
    c
}

fn decode_response(host: &PhytiumMci, resp_type: ResponseType) -> Result<Response, Error> {
    let resp = host.regs.resp().read();
    Ok(match resp_type {
        ResponseType::None => Response::Empty,
        ResponseType::R1 => Response::R1(R1Response { raw: resp[0] }),
        ResponseType::R1b => Response::R1b(R1Response { raw: resp[0] }),
        ResponseType::R2 => Response::R2(read_r2(resp)),
        ResponseType::R3 => Response::R3(OcrResponse::from_raw(resp[0])),
        ResponseType::R4 => Response::R4(SdioOcrResponse::from_raw(resp[0])),
        ResponseType::R5 => Response::R5(SdioRwResponse::from_raw(resp[0])),
        ResponseType::R6 => Response::R6(RcaResponse::from_raw(resp[0])),
        ResponseType::R7 => Response::R7(IfCondResponse::from_raw(resp[0])),
        // Future ResponseType variants are not decoded by this controller;
        // surface as UnsupportedCommand instead of returning silent zeros.
        _ => return Err(Error::UnsupportedCommand),
    })
}

fn read_r2(resp: [u32; 4]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&resp[3].to_be_bytes());
    bytes[4..8].copy_from_slice(&resp[2].to_be_bytes());
    bytes[8..12].copy_from_slice(&resp[1].to_be_bytes());
    bytes[12..16].copy_from_slice(&resp[0].to_be_bytes());
    bytes
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use sdmmc_protocol::cmd::cmd17;

    use super::*;
    use crate::host::PendingData;

    #[test]
    fn data_command_preserves_prepared_irq_generation() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        host.irq.state.begin_request();
        let prepared_generation = host.irq.state.generation();
        host.pending_data = Some(PendingData {
            direction: DataDirection::Read,
            block_size: 512,
            block_count: 1,
        });

        host.submit_command(&cmd17(0)).unwrap();

        assert_eq!(host.irq.state.generation(), prepared_generation);
    }

    #[test]
    fn idmac_data_command_uses_block_transfer_mode() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        host.irq.state.begin_request();
        host.pending_data = Some(PendingData {
            direction: DataDirection::Read,
            block_size: 64,
            block_count: 1,
        });

        host.submit_command(&ProtoCmd::new(6, 0, ResponseType::R1))
            .unwrap();

        assert!(!host.regs.cmd().read().stream_mode());
    }
}
