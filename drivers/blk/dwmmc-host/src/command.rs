//! Command issue and response decoding.
//!
//! Encodes [`sdmmc_protocol::cmd::Command`] into a DW_mshc CMD register
//! value, fires it, polls RINTSTS for completion, and decodes the four
//! 32-bit response slots back into [`Response`].

use log::warn;
use sdmmc_protocol::{
    CommandPoll, CommandResponsePoll,
    cmd::{Command as ProtoCmd, DataDirection},
    error::{Error, ErrorContext, Phase},
    response::{
        IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType,
        SdioOcrResponse, SdioRwResponse,
    },
};

use crate::{
    host::DwMmc,
    regs::{Cmd, RegisterBlockVolatileFieldAccess},
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
    Complete {
        response: Response,
    },
    Failed {
        error: Error,
    },
}

impl DwMmc {
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
        if !matches!(self.command_state, CommandState::Idle) {
            return Err(Error::UnsupportedCommand);
        }
        if !self.card_present() {
            return Err(Error::NoCard);
        }
        let data = self.pending_data.take();
        self.prepare_irq_for_request();
        self.command_state = CommandState::WaitingInhibit {
            cmd: *cmd,
            data,
            polls: 0,
        };
        if let Err(err) = self.poll_command() {
            self.command_state = CommandState::Idle;
            return Err(err);
        }
        Ok(())
    }

    pub fn poll_command(&mut self) -> Result<CommandPoll, Error> {
        match self.command_state {
            CommandState::WaitingInhibit { cmd, data, polls } => {
                if !self.command_can_issue(data.is_some()) {
                    if polls >= COMMAND_WAIT_POLLS {
                        self.log_command_timeout("wait-inhibit", cmd);
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
                    return Ok(CommandPoll::Pending);
                }
                self.program_command(&cmd, data);
                return Ok(CommandPoll::Pending);
            }
            CommandState::WaitingStart { cmd, polls } => {
                if self.regs.cmd().read().start_cmd() {
                    if polls >= COMMAND_WAIT_POLLS {
                        self.log_command_timeout("wait-start", cmd);
                        let err =
                            Error::Timeout(ErrorContext::for_cmd(Phase::CommandSend, cmd.index));
                        self.command_state = CommandState::Failed { error: err };
                        return Err(err);
                    }
                    self.command_state = CommandState::WaitingStart {
                        cmd,
                        polls: polls + 1,
                    };
                    return Ok(CommandPoll::Pending);
                }
                self.command_state = CommandState::Issued { cmd, polls: 0 };
                return Ok(CommandPoll::Pending);
            }
            CommandState::Issued { .. } => {}
            CommandState::Complete { .. } => return Ok(CommandPoll::Complete),
            CommandState::Failed { error } => return Err(error),
            CommandState::Idle => return Err(Error::InvalidArgument),
        }

        let CommandState::Issued { cmd, polls } = self.command_state else {
            unreachable!();
        };
        let raw_status = self.take_command_irq_status();
        let status = crate::regs::RIntSts::from_bits(raw_status);
        if status.error() {
            let err = self.translate_int_error(status, Phase::ResponseWait, cmd.index);
            self.log_command_error("interrupt-error", cmd, raw_status, err);
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        if status.command_done() {
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
            self.command_state = CommandState::Complete { response };
            return Ok(CommandPoll::Complete);
        }
        if polls >= COMMAND_WAIT_POLLS {
            let err = Error::Timeout(ErrorContext::for_cmd(Phase::ResponseWait, cmd.index));
            self.log_command_error("response-timeout", cmd, raw_status, err);
            self.command_state = CommandState::Failed { error: err };
            return Err(err);
        }
        self.command_state = CommandState::Issued {
            cmd,
            polls: polls + 1,
        };
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
            | CommandState::Issued { .. } => Err(Error::InvalidArgument),
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
        let data_dir = data.map(|d| {
            self.program_data_phase(d.block_size, d.block_count);
            d.direction
        });
        self.regs.cmdarg().write(cmd.argument);
        self.regs.cmd().write(encode_command(cmd, data_dir));
        self.command_state = CommandState::WaitingStart {
            cmd: *cmd,
            polls: 0,
        };
    }

    fn take_command_irq_status(&mut self) -> u32 {
        if self.completion_irq_enabled() {
            return self
                .irq
                .state
                .take(crate::DWMMC_INT_COMMAND_DONE | crate::DWMMC_INT_ERROR_MASK);
        }
        let raw_status = self.regs.rintsts().read().into_bits();
        let consume = raw_status & (crate::DWMMC_INT_COMMAND_DONE | crate::DWMMC_INT_ERROR_MASK);
        if consume != 0 {
            self.regs
                .rintsts()
                .write(crate::regs::RIntSts::from_bits(consume));
        }
        raw_status
    }

    fn clear_command_int_status(&mut self) {
        let raw_status = self.regs.rintsts().read().into_bits()
            & (crate::DWMMC_INT_COMMAND_DONE | crate::DWMMC_INT_ERROR_MASK);
        if raw_status != 0 {
            self.regs
                .rintsts()
                .write(crate::regs::RIntSts::from_bits(raw_status));
        }
        self.irq
            .state
            .clear(crate::DWMMC_INT_COMMAND_DONE | crate::DWMMC_INT_ERROR_MASK);
    }

    fn log_command_timeout(&self, stage: &str, cmd: ProtoCmd) {
        warn!(
            "dwmmc-command: {stage} timeout cmd={} arg={:#010x} resp={:?} cmdreg={:#010x} \
             status={:#010x} rintsts={:#010x} mintsts={:#010x} intmask={:#010x} ctrl={:#010x} \
             pwren={:#010x} cdetect={:#010x} clkena={:#010x} clksrc={:#010x} clkdiv={:#010x} \
             ctype={:#010x} uhs={:#010x} rst={:#010x}",
            cmd.index,
            cmd.argument,
            cmd.response,
            self.regs.cmd().read().into_bits(),
            self.regs.status().read().into_bits(),
            self.regs.rintsts().read().into_bits(),
            self.regs.mintsts().read(),
            self.regs.intmask().read(),
            self.regs.ctrl().read().into_bits(),
            self.regs.pwren().read(),
            self.regs.cdetect().read(),
            self.regs.clkena().read().into_bits(),
            self.regs.clksrc().read(),
            self.regs.clkdiv().read().into_bits(),
            self.regs.ctype().read().into_bits(),
            self.regs.uhs().read().into_bits(),
            self.regs.rst().read(),
        );
    }

    fn log_command_error(&self, stage: &str, cmd: ProtoCmd, raw_status: u32, err: Error) {
        warn!(
            "dwmmc-command: {stage} cmd={} arg={:#010x} resp={:?} raw={:#010x} err={:?} \
             cmdreg={:#010x} status={:#010x} rintsts={:#010x} mintsts={:#010x} intmask={:#010x} \
             ctrl={:#010x} pwren={:#010x} cdetect={:#010x} clkena={:#010x} clksrc={:#010x} \
             clkdiv={:#010x} ctype={:#010x} uhs={:#010x} rst={:#010x}",
            cmd.index,
            cmd.argument,
            cmd.response,
            raw_status,
            err,
            self.regs.cmd().read().into_bits(),
            self.regs.status().read().into_bits(),
            self.regs.rintsts().read().into_bits(),
            self.regs.mintsts().read(),
            self.regs.intmask().read(),
            self.regs.ctrl().read().into_bits(),
            self.regs.pwren().read(),
            self.regs.cdetect().read(),
            self.regs.clkena().read().into_bits(),
            self.regs.clksrc().read(),
            self.regs.clkdiv().read().into_bits(),
            self.regs.ctype().read().into_bits(),
            self.regs.uhs().read().into_bits(),
            self.regs.rst().read(),
        );
    }

    fn prepare_irq_for_request(&mut self) {
        self.clear_all_int_status();
        self.irq.state.begin_request();
    }

    pub(crate) fn abort_command(&mut self) -> Result<(), Error> {
        self.clear_command_int_status();
        for _ in 0..COMMAND_WAIT_POLLS {
            if !self.regs.cmd().read().start_cmd() {
                self.clear_all_int_status();
                self.reset_and_init_preserving_irq()?;
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

/// Build the CMD register value for a single command.
///
/// `data_dir` is `Some` when the command carries a data phase (the
/// protocol layer signals this by populating
/// [`crate::host::PendingData`]). The direction selects `read_write`;
/// passing `None` issues a non-data command.
fn encode_command(cmd: &ProtoCmd, data_dir: Option<DataDirection>) -> Cmd {
    // Defaults: start_cmd=1, wait_prvdata_complete=1, use_hold_reg=1.
    // CMD0 needs send_initialization=1; everything else leaves it 0.
    let mut c = Cmd::new()
        .with_start_cmd(true)
        .with_use_hold_reg(true)
        .with_wait_prvdata_complete(true)
        .with_cmd_index(cmd.index & 0x3F);

    match cmd.response {
        ResponseType::None => {
            // No response_expect; no CRC check.
        }
        ResponseType::R1 | ResponseType::R5 | ResponseType::R6 | ResponseType::R7 => {
            c = c.with_response_expect(true).with_check_response_crc(true);
        }
        ResponseType::R1b => {
            // R1b is short response with busy. The DW_mshc treats the
            // busy hold-off through the same CMD bits as R1 plus the
            // controller's own data-busy gating; no separate
            // "response_length_busy" flag exists.
            c = c.with_response_expect(true).with_check_response_crc(true);
        }
        ResponseType::R2 => {
            // R2 = long (136-bit) response. CRC of the on-bus frame is
            // checked against the spec's R2 polynomial.
            c = c
                .with_response_expect(true)
                .with_response_length(true)
                .with_check_response_crc(true);
        }
        ResponseType::R3 | ResponseType::R4 => {
            // OCR responses don't include a valid CRC; check_response_crc
            // must be 0 or the controller will flag every R3/R4 as a
            // CRC error.
            c = c.with_response_expect(true);
        }
        // Future ResponseType variants land here as bare command; controller default is no response_expect.
        _ => {}
    }

    if cmd.index == 0 {
        // Power-up cards need 80 init clocks before CMD0.
        c = c.with_send_initialization(true);
    }

    if let Some(dir) = data_dir {
        c = c.with_data_expected(true);
        // The data-command submit path carries direction explicitly because
        // some indices are overloaded (CMD6 = ACMD6 SET_BUS_WIDTH no-data /
        // SWITCH_FUNC read; CMD8 = SEND_IF_COND no-data on SD /
        // SEND_EXT_CSD read on MMC). We trust that signal here rather than
        // inferring from `cmd.index`.
        if matches!(dir, DataDirection::Write) {
            c = c.with_read_write(true);
        }
    }

    c
}

fn decode_response(host: &DwMmc, resp_type: ResponseType) -> Result<Response, Error> {
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
        // Future ResponseType variants are not decoded by this controller.
        _ => return Err(Error::UnsupportedCommand),
    })
}

/// Reorder the four 32-bit response slots into the 16-byte buffer the
/// protocol layer's CID/CSD parsers expect.
///
/// On DW_mshc, RESP[3..0] holds the on-bus 128-bit R2 frame with the
/// MSB of card_resp at the top of RESP3 (header / start bits are
/// stripped by the CIU before storage). The protocol layer's
/// `CidResponse::from_raw` / `CsdResponse::from_raw` expect byte 0 =
/// card_resp[127:120] (i.e. MID for CID), so we serialize each u32
/// big-endian and place RESP3 at offset 0.
fn read_r2(resp: [u32; 4]) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&resp[3].to_be_bytes());
    bytes[4..8].copy_from_slice(&resp[2].to_be_bytes());
    bytes[8..12].copy_from_slice(&resp[1].to_be_bytes());
    bytes[12..16].copy_from_slice(&resp[0].to_be_bytes());
    bytes
}
