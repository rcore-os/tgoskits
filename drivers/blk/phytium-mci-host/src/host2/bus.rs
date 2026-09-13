use super::*;

pub struct BusRequest {
    pub(super) owner: usize,
    pub(super) id: u64,
    pub(super) done: bool,
    pub(super) state: BusRequestState,
}

impl BusRequest {
    pub(super) fn pending(owner: usize, id: u64, state: BusRequestState) -> Self {
        Self {
            owner,
            id,
            done: false,
            state,
        }
    }
}

pub(super) enum BusRequestState {
    ResetAll {
        state: PhytiumResetState,
        restore_completion_irq: bool,
    },
    ResetDataLine {
        started: bool,
        polls: u32,
    },
    PowerOn,
    PowerOff,
    SetClock(PhytiumClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(PhytiumVoltageState),
}

pub(super) enum PhytiumResetState {
    Start,
    WaitReset { polls: u32 },
    WaitIdmacReset { polls: u32 },
    InitClock(PhytiumClockState),
}

pub(super) enum PhytiumClockState {
    Start {
        timing: timing::TimingTable,
    },
    WaitExternalClock {
        polls: u32,
        timing: timing::TimingTable,
    },
    WaitDisable {
        polls: u32,
        timing: timing::TimingTable,
    },
    ProgramDivider {
        timing: timing::TimingTable,
    },
    WaitEnable {
        polls: u32,
    },
}

pub(super) enum PhytiumVoltageState {
    Start(SignalVoltage),
    WaitUpdate { polls: u32 },
}

const PHYTIUM_RESET_POLLS: u32 = 1_000_000;
const PHYTIUM_CLOCK_POLLS: u32 = 1_000_000;

impl PhytiumMci {
    pub(super) fn prepare_host2_bus_op(
        &self,
        op: sdmmc_host::BusOp,
    ) -> Result<BusRequestState, sdmmc_host::Error> {
        match op {
            sdmmc_host::BusOp::ResetAll => Ok(BusRequestState::ResetAll {
                state: PhytiumResetState::Start,
                restore_completion_irq: self.completion_irq_enabled(),
            }),
            sdmmc_host::BusOp::ResetCommandLine => Err(sdmmc_host::Error::Unsupported),
            sdmmc_host::BusOp::ResetDataLine => Ok(BusRequestState::ResetDataLine {
                started: false,
                polls: 0,
            }),
            sdmmc_host::BusOp::PowerOn => Ok(BusRequestState::PowerOn),
            sdmmc_host::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdmmc_host::BusOp::SetClock(speed) => {
                let timing =
                    timing::TimingTable::sd_for_speed(speed).map_err(map_protocol_error)?;
                Ok(BusRequestState::SetClock(PhytiumClockState::Start {
                    timing,
                }))
            }
            sdmmc_host::BusOp::SetClockHz(_) => Err(sdmmc_host::Error::Unsupported),
            sdmmc_host::BusOp::SetBusWidth(width) => Ok(BusRequestState::SetBusWidth(width)),
            sdmmc_host::BusOp::SetSignalVoltage(voltage) => {
                uhs_bits_after_voltage(self.regs.uhs().read(), voltage)
                    .map_err(map_protocol_error)?;
                Ok(BusRequestState::SetSignalVoltage(
                    PhytiumVoltageState::Start(voltage),
                ))
            }
            sdmmc_host::BusOp::ExecuteTuning { .. } => Err(sdmmc_host::Error::Unsupported),
            _ => Err(sdmmc_host::Error::Unsupported),
        }
    }

    pub(super) fn advance_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        match state {
            BusRequestState::ResetAll {
                state,
                restore_completion_irq,
            } => self.advance_host2_reset_all(state, *restore_completion_irq),
            BusRequestState::ResetDataLine { started, polls } => {
                self.advance_host2_fifo_reset(started, polls)
            }
            BusRequestState::PowerOn => {
                self.regs.pwren().write(1);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::PowerOff => {
                self.regs.pwren().write(0);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.advance_host2_clock(clock),
            BusRequestState::SetBusWidth(width) => {
                PhytiumMci::set_bus_width(self, *width);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => self.advance_host2_voltage(voltage),
        }
    }

    fn advance_host2_reset_all(
        &mut self,
        state: &mut PhytiumResetState,
        restore_completion_irq: bool,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        match state {
            PhytiumResetState::Start => {
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.ctrl().update(|r| {
                    r.with_use_internal_dmac(false)
                        .with_dma_enable(false)
                        .with_int_enable(false)
                });
                self.regs.ctrl().update(|r| {
                    r.with_controller_reset(true)
                        .with_fifo_reset(true)
                        .with_dma_reset(true)
                });
                *state = PhytiumResetState::WaitReset { polls: 0 };
                Ok(register_pending())
            }
            PhytiumResetState::WaitReset { polls } => {
                let ctrl = self.regs.ctrl().read();
                if !ctrl.controller_reset() && !ctrl.fifo_reset() && !ctrl.dma_reset() {
                    self.regs.intmask().write(0);
                    self.regs.idinten().write(0);
                    self.clear_all_int_status();
                    self.regs.idsts().write(u32::MAX);
                    self.irq.state.clear_all();
                    self.regs.ctype().write(crate::regs::CType::new());
                    self.regs.uhs().write(crate::regs::Uhs::new());
                    self.regs.tmout().write(0xffff_ffff);
                    self.regs.pwren().write(1);
                    self.regs.fifoth().write(crate::host::FIFO_THRESHOLD);
                    self.write_ext_reg(
                        crate::regs::CARD_THRCTL_OFFSET,
                        crate::host::CARD_READ_THRESHOLD_ENABLE
                            | crate::host::CARD_READ_THRESHOLD_DEPTH8,
                    );
                    self.start_idmac_reset();
                    *state = PhytiumResetState::WaitIdmacReset { polls: 0 };
                    return Ok(register_pending());
                }
                if *polls >= PHYTIUM_RESET_POLLS {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(register_pending())
            }
            PhytiumResetState::WaitIdmacReset { polls } => {
                if self.idmac_reset_complete() {
                    if let Some(ring) = self.idmac_ring.as_mut() {
                        ring.clear_after_reset();
                    }
                    self.dma_poisoned = false;
                    *state = PhytiumResetState::InitClock(PhytiumClockState::Start {
                        timing: timing::TimingTable::sd_for_speed(ClockSpeed::Identification)
                            .map_err(map_protocol_error)?,
                    });
                    return Ok(register_pending());
                }
                if *polls >= PHYTIUM_RESET_POLLS {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(register_pending())
            }
            PhytiumResetState::InitClock(clock) => {
                let progress = self.advance_host2_clock(clock)?;
                if restore_completion_irq
                    && matches!(progress, sdmmc_host::RequestProgress::Complete(Ok(())))
                {
                    self.enable_completion_irq();
                }
                Ok(progress)
            }
        }
    }

    fn advance_host2_fifo_reset(
        &mut self,
        started: &mut bool,
        polls: &mut u32,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        if !*started {
            self.regs.ctrl().update(|r| r.with_fifo_reset(true));
            *started = true;
        }
        if !self.regs.ctrl().read().fifo_reset() {
            return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
        }
        if *polls >= PHYTIUM_RESET_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::DataRead,
            ))));
        }
        *polls += 1;
        Ok(register_pending())
    }

    fn advance_host2_clock(
        &mut self,
        state: &mut PhytiumClockState,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        match state {
            PhytiumClockState::Start { timing } => {
                self.use_hold_reg = timing.use_hold;
                self.write_ext_reg(crate::regs::CLK_SRC_OFFSET, 0);
                self.write_ext_reg(crate::regs::CLK_SRC_OFFSET, timing.clk_src);
                *state = PhytiumClockState::WaitExternalClock {
                    polls: 0,
                    timing: *timing,
                };
                Ok(register_pending())
            }
            PhytiumClockState::WaitExternalClock { polls, timing } => {
                if self.regs.cksts().read().ready() {
                    self.regs.clkena().write(crate::regs::ClkEna::new());
                    self.start_update_clock(false);
                    *state = PhytiumClockState::WaitDisable {
                        polls: 0,
                        timing: *timing,
                    };
                    return Ok(register_pending());
                }
                if *polls >= PHYTIUM_CLOCK_POLLS {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(register_pending())
            }
            PhytiumClockState::WaitDisable { polls, timing } => {
                if self.update_clock_complete(polls)? {
                    *state = PhytiumClockState::ProgramDivider { timing: *timing };
                }
                Ok(register_pending())
            }
            PhytiumClockState::ProgramDivider { timing } => {
                self.program_clock_dividers(*timing);
                self.regs
                    .clkena()
                    .write(crate::regs::ClkEna::new().with_cclk_enable(1));
                self.start_update_clock(false);
                *state = PhytiumClockState::WaitEnable { polls: 0 };
                Ok(register_pending())
            }
            PhytiumClockState::WaitEnable { polls } => {
                if self.update_clock_complete(polls)? {
                    return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
                }
                Ok(register_pending())
            }
        }
    }

    fn advance_host2_voltage(
        &mut self,
        state: &mut PhytiumVoltageState,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        match state {
            PhytiumVoltageState::Start(voltage) => {
                let next = uhs_bits_after_voltage(self.regs.uhs().read(), *voltage)
                    .map_err(map_protocol_error)?;
                self.regs.uhs().write(next);
                self.start_update_clock(matches!(*voltage, SignalVoltage::V180));
                *state = PhytiumVoltageState::WaitUpdate { polls: 0 };
                Ok(register_pending())
            }
            PhytiumVoltageState::WaitUpdate { polls } => {
                if self.update_clock_complete(polls)? {
                    return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
                }
                Ok(register_pending())
            }
        }
    }

    fn start_update_clock(&self, voltage_switch: bool) {
        self.regs.cmd().write(
            crate::regs::Cmd::new()
                .with_start_cmd(true)
                .with_wait_prvdata_complete(true)
                .with_update_clock_registers_only(true)
                .with_volt_switch(voltage_switch),
        );
    }

    fn update_clock_complete(&self, polls: &mut u32) -> Result<bool, sdmmc_host::Error> {
        if !self.regs.cmd().read().start_cmd() {
            return Ok(true);
        }
        if *polls >= PHYTIUM_CLOCK_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        *polls += 1;
        Ok(false)
    }
    pub(super) fn check_host2_bus_request(
        &self,
        request: &BusRequest,
    ) -> Result<(), sdmmc_host::AdvanceRequestError> {
        if request.done {
            return Err(sdmmc_host::AdvanceRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdmmc_host::AdvanceRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdmmc_host::AdvanceRequestError::StaleGeneration);
        }
        Ok(())
    }
    pub(super) fn complete_host2_bus_request(&mut self, request: &mut BusRequest) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    pub(super) fn abort_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<(), sdmmc_host::Error> {
        match state {
            BusRequestState::ResetAll { .. }
            | BusRequestState::SetClock(_)
            | BusRequestState::SetSignalVoltage(_) => {
                self.reset_and_init_preserving_irq()
                    .map_err(map_protocol_error)?;
            }
            BusRequestState::ResetDataLine { started, .. } if *started => {
                self.reset_fifo(sdmmc_protocol::Phase::DataRead)
                    .map_err(map_protocol_error)?;
            }
            BusRequestState::PowerOn
            | BusRequestState::PowerOff
            | BusRequestState::SetBusWidth(_) => {}
            BusRequestState::ResetDataLine { .. } => {}
        }
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.command_state = command::CommandState::Idle;
        Ok(())
    }
}
