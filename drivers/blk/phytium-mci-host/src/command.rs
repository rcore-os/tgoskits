use sdmmc_protocol::{
    CommandPoll, CommandResponsePoll,
    cmd::{Command as ProtoCmd, DataDirection},
    error::{Error, Phase},
    response::{
        IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType,
        SdioOcrResponse, SdioRwResponse,
    },
};

use crate::{
    host::{PhytiumMci, RegisterOwner},
    regs::{Cmd, RIntSts, RegisterBlockVolatileFieldAccess},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum CommandState {
    Idle,
    WaitingInhibit {
        cmd: ProtoCmd,
        data: Option<crate::host::PendingData>,
    },
    WaitingStart {
        cmd: ProtoCmd,
    },
    Issued {
        cmd: ProtoCmd,
    },
    WaitingBusy {
        cmd: ProtoCmd,
        response: Response,
    },
    Complete {
        response: Response,
    },
    Failed {
        error: Error,
    },
}

impl PhytiumMci {
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

    pub fn submit_command(&mut self, cmd: &ProtoCmd) -> Result<(), Error> {
        let irq = self.irq.clone();
        let register_owner = irq.state.try_begin_task_update().ok_or(Error::Busy)?;
        self.submit_command_while_registers_owned(cmd, &register_owner)
    }

    pub(crate) fn ensure_runtime_data_command_can_issue(&self) -> Result<(), Error> {
        if !self.completion_irq_enabled() {
            return Err(Error::Busy);
        }
        if !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        if !self.command_can_issue(true) {
            return Err(Error::Busy);
        }
        Ok(())
    }

    /// Stage and activate a command while the caller owns the controller's
    /// task-side register gate.
    pub(crate) fn submit_command_while_registers_owned(
        &mut self,
        cmd: &ProtoCmd,
        _register_owner: &RegisterOwner<'_>,
    ) -> Result<(), Error> {
        if !self.completion_irq_enabled() {
            return Err(Error::Busy);
        }
        if !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        if !self.command_can_issue(self.pending_data.is_some()) {
            return Err(Error::Busy);
        }
        let data = self.pending_data.take();
        self.prepare_irq_for_request();
        self.command_state = CommandState::WaitingInhibit { cmd: *cmd, data };
        if let Err(err) = self.poll_command() {
            self.command_state = CommandState::Idle;
            return Err(err);
        }
        Ok(())
    }

    /// Activate a data command after its DMA engine owns the request backing.
    ///
    /// Every fallible admission check must happen before this transition.
    /// Holding `register_owner` excludes the IRQ endpoint until the request
    /// generation and command doorbell have both been published.
    pub(crate) fn activate_admitted_data_command(
        &mut self,
        cmd: &ProtoCmd,
        data: crate::host::PendingData,
        _register_owner: &RegisterOwner<'_>,
    ) {
        debug_assert!(matches!(self.command_state, CommandState::Idle));
        debug_assert!(self.pending_data.is_none());
        self.prepare_irq_for_request();
        self.program_command(cmd, Some(data));
    }

    pub fn poll_command(&mut self) -> Result<CommandPoll, Error> {
        match self.command_state {
            CommandState::WaitingInhibit { cmd, data } => {
                if !self.command_can_issue(data.is_some()) {
                    return Ok(CommandPoll::Pending);
                }
                self.program_command(&cmd, data);
                return Ok(CommandPoll::Pending);
            }
            CommandState::WaitingStart { cmd } => {
                if self.regs.cmd().read().start_cmd() {
                    return Ok(CommandPoll::Pending);
                }
                self.command_state = CommandState::Issued { cmd };
            }
            CommandState::Issued { .. } => {}
            CommandState::WaitingBusy { cmd, response } => {
                return self.poll_r1b_busy(cmd, response);
            }
            CommandState::Complete { .. } => return Ok(CommandPoll::Complete),
            CommandState::Failed { error } => return Err(error),
            CommandState::Idle => return Err(Error::InvalidArgument),
        }

        let CommandState::Issued { cmd } = self.command_state else {
            unreachable!();
        };
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
                self.command_state = CommandState::WaitingBusy { cmd, response };
                return self.poll_r1b_busy(cmd, response);
            }
            self.command_state = CommandState::Complete { response };
            return Ok(CommandPoll::Complete);
        }
        Ok(CommandPoll::Pending)
    }

    fn poll_r1b_busy(&mut self, cmd: ProtoCmd, response: Response) -> Result<CommandPoll, Error> {
        let raw_status = self.take_busy_irq_status();
        let status = RIntSts::from_bits(raw_status);
        if status.error() {
            let error = self.translate_int_error(status, Phase::ResponseWait, cmd.index);
            self.command_state = CommandState::Failed { error };
            return Err(error);
        }
        if status.data_transfer_over() {
            self.command_state = CommandState::Complete { response };
            return Ok(CommandPoll::Complete);
        }
        Ok(CommandPoll::Pending)
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
        self.clear_command_int_status();
        let mut use_idmac = false;
        let data_dir = data.map(|d| {
            self.program_data_phase(d.block_size, d.block_count);
            use_idmac = d.use_idmac;
            d.direction
        });
        self.regs.cmdarg().write(cmd.argument);
        let mut encoded = encode_command(cmd, data_dir).with_use_hold_reg(self.use_hold_reg);
        if use_idmac {
            encoded = encoded.with_transfer_mode(true);
        }
        self.regs.cmd().write(encoded);
        self.command_state = CommandState::WaitingStart { cmd: *cmd };
    }

    fn take_command_irq_status(&mut self) -> u32 {
        self.irq
            .state
            .take_status(crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_ERROR_MASK)
    }

    fn take_busy_irq_status(&mut self) -> u32 {
        self.irq
            .state
            .take_status(crate::MCI_INT_DATA_TRANSFER_OVER | crate::MCI_INT_ERROR_MASK)
    }

    fn clear_command_int_status(&mut self) {
        self.irq
            .state
            .clear_status(crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_ERROR_MASK);
    }

    fn prepare_irq_for_request(&mut self) {
        self.irq.state.begin_request();
    }

    pub(crate) fn abort_command(&mut self) -> Result<(), Error> {
        if !self.recovery_quiesced {
            return Err(Error::Busy);
        }
        self.clear_command_int_status();
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.data_cmd_index = 0;
        self.command_state = CommandState::Idle;
        Ok(())
    }
}

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

    use sdmmc_protocol::{CommandPoll, cmd::CMD12};

    use super::*;

    #[test]
    fn runtime_request_setup_does_not_ack_raw_irq_status() {
        const RINTSTS_WORD: usize = 17;
        const IDSTS_WORD: usize = 36;
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        let pending = crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_DATA_TRANSFER_OVER;
        let pending_idmac = crate::MCI_IDSTS_RECEIVE;
        mmio[RINTSTS_WORD] = pending;
        mmio[IDSTS_WORD] = pending_idmac;
        host.enable_completion_irq();

        host.prepare_irq_for_request();

        assert_eq!(mmio[RINTSTS_WORD], pending);
        assert_eq!(mmio[IDSTS_WORD], pending_idmac);
        assert_ne!(host.irq.state.generation(), 0);
    }

    #[test]
    fn r1b_command_done_without_busy_release_remains_pending() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        host.enable_completion_irq();
        host.irq.state.begin_request();
        host.command_state = CommandState::Issued { cmd: CMD12 };
        let generation = host.irq.state.generation();
        host.irq
            .state
            .cache_if_current(generation, crate::MCI_INT_COMMAND_DONE, 0);

        assert_eq!(host.poll_command(), Ok(CommandPoll::Pending));
    }

    #[test]
    fn merged_r1b_command_and_busy_release_complete_in_one_irq_snapshot() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { PhytiumMci::new(base) };
        host.enable_completion_irq();
        host.irq.state.begin_request();
        host.command_state = CommandState::Issued { cmd: CMD12 };
        let generation = host.irq.state.generation();
        host.irq.state.cache_if_current(
            generation,
            crate::MCI_INT_COMMAND_DONE | crate::MCI_INT_DATA_TRANSFER_OVER,
            0,
        );

        assert_eq!(host.poll_command(), Ok(CommandPoll::Complete));
    }
}
