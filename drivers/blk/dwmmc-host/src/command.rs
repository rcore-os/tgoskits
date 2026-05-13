//! Command issue and response decoding.
//!
//! Encodes [`sdmmc_protocol::cmd::Command`] into a DW_mshc CMD register
//! value, fires it, polls RINTSTS for completion, and decodes the four
//! 32-bit response slots back into [`Response`].

use sdmmc_protocol::{
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

const POLL_LIMIT: u32 = 1_000_000;

impl DwMmc {
    /// Issue one command and decode its response.
    ///
    /// When the protocol layer has armed a data phase via
    /// `prepare_data_transfer` on the trait surface, the command is
    /// issued with `data_expected` set and the BLKSIZ/BYTCNT registers
    /// pre-programmed; the data phase itself is then driven through
    /// the FIFO by `pio_read` / `pio_write`.
    pub fn issue_command(&mut self, cmd: &ProtoCmd) -> Result<Response, Error> {
        let data = self.pending_data.take();
        if data.is_some() {
            self.data_cmd_index = cmd.cmd;
        }

        // 1. Wait for the CIU's command FSM to be idle. If a data
        //    phase is pending we also block on the data line, which
        //    matches the `wait_prvdata_complete` semantics the CMD
        //    register already enforces — but the pre-check makes the
        //    failure mode (timeout vs. hardware-locked) easier to
        //    distinguish.
        self.wait_inhibit(data.is_some(), cmd.cmd)?;

        // 2. Clear any leftover RINTSTS bits so the polls below only
        //    fire on this command's events.
        self.clear_all_int_status();

        // 3. If data is pending, program the data-phase registers
        //    *before* writing CMD. The CIU latches BLKSIZ/BYTCNT at
        //    start_cmd time.
        let data_dir = data.map(|d| {
            self.program_data_phase(d.block_size, d.block_count);
            d.direction
        });

        // 4. Argument first, then CMD — start_cmd in CMD is what
        //    actually fires the transaction.
        self.regs.cmdarg().write(cmd.arg);
        let encoded = encode_command(cmd, data_dir);
        self.regs.cmd().write(encoded);

        // 5. Wait for start_cmd to clear (CIU accepted the command).
        for _ in 0..POLL_LIMIT {
            if !self.regs.cmd().read().start_cmd() {
                break;
            }
            core::hint::spin_loop();
        }

        // 6. Wait for command_done (or an error). `R1b` and data-bearing
        //    commands have additional waits but those happen in the data
        //    path; here we only block on the response.
        if matches!(cmd.resp_type, ResponseType::None) {
            // No response: still wait for command_done so we know the
            // CIU finished clocking out the command frame.
            self.wait_command_done(cmd.cmd)?;
        } else {
            self.wait_command_done(cmd.cmd)?;
        }

        decode_response(self, cmd.resp_type)
    }

    /// Block until [`crate::regs::RIntSts::data_transfer_over`] fires (or
    /// an error). Used by [`crate::data`] after the last block has been
    /// drained / pushed.
    pub(crate) fn wait_data_transfer_over(&self, cmd_index: u8, phase: Phase) -> Result<(), Error> {
        for _ in 0..POLL_LIMIT {
            let s = self.regs.rintsts().read();
            if s.error() {
                return Err(self.translate_int_error(s, phase, cmd_index));
            }
            if s.data_transfer_over() {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::for_cmd(phase, cmd_index)))
    }

    fn wait_inhibit(&self, has_data: bool, cmd_index: u8) -> Result<(), Error> {
        for _ in 0..POLL_LIMIT {
            let cmd_busy = self.regs.cmd().read().start_cmd();
            let data_busy = has_data && self.regs.status().read().data_busy();
            if !cmd_busy && !data_busy {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::for_cmd(
            Phase::CommandSend,
            cmd_index,
        )))
    }

    fn wait_command_done(&self, cmd_index: u8) -> Result<(), Error> {
        for _ in 0..POLL_LIMIT {
            let s = self.regs.rintsts().read();
            if s.error() {
                return Err(self.translate_int_error(s, Phase::ResponseWait, cmd_index));
            }
            if s.command_done() {
                // Acknowledge command_done so the next command starts
                // clean. RXDR/TXDR/DTO bits stay armed for the data path.
                let mut ack = crate::regs::RIntSts::new();
                ack.set_command_done(true);
                self.regs.rintsts().write(ack);
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(Error::Timeout(ErrorContext::for_cmd(
            Phase::ResponseWait,
            cmd_index,
        )))
    }
}

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
        .with_cmd_index(cmd.cmd & 0x3F);

    match cmd.resp_type {
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
    }

    if cmd.cmd == 0 {
        // Power-up cards need 80 init clocks before CMD0.
        c = c.with_send_initialization(true);
    }

    if let Some(dir) = data_dir {
        c = c.with_data_expected(true);
        // The protocol layer signals direction explicitly via
        // `prepare_data_transfer` because some indices are
        // overloaded (CMD6 = ACMD6 SET_BUS_WIDTH no-data /
        // SWITCH_FUNC read; CMD8 = SEND_IF_COND no-data on SD /
        // SEND_EXT_CSD read on MMC). We trust that signal here
        // rather than inferring from `cmd.cmd`.
        if matches!(dir, DataDirection::Write) {
            c = c.with_read_write(true);
        }
    }

    c
}

fn decode_response(host: &DwMmc, resp_type: ResponseType) -> Result<Response, Error> {
    let resp = host.regs.resp().read();
    Ok(match resp_type {
        ResponseType::None => Response::None,
        ResponseType::R1 => Response::R1(R1Response { raw: resp[0] }),
        ResponseType::R1b => Response::R1b(R1Response { raw: resp[0] }),
        ResponseType::R2 => Response::R2(read_r2(resp)),
        ResponseType::R3 => Response::R3(OcrResponse::from_raw(resp[0])),
        ResponseType::R4 => Response::R4(SdioOcrResponse::from_raw(resp[0])),
        ResponseType::R5 => Response::R5(SdioRwResponse::from_raw(resp[0])),
        ResponseType::R6 => Response::R6(RcaResponse::from_raw(resp[0])),
        ResponseType::R7 => Response::R7(IfCondResponse::from_raw(resp[0])),
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
