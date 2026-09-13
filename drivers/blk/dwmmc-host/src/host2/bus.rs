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
        state: DwMmcResetState,
        restore_completion_irq: bool,
    },
    ResetDataLine {
        started: bool,
        polls: u32,
    },
    PowerOn {
        state: DwMmcResetState,
        restore_completion_irq: bool,
    },
    PowerOff,
    SetClock(DwMmcClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(SignalVoltage),
}

pub(super) enum DwMmcResetState {
    Start,
    WaitReset { polls: u32 },
    WaitDmaRequest { polls: u32 },
    WaitSecondFifoReset { polls: u32 },
}

pub(super) enum DwMmcClockState {
    Start {
        speed: Option<ClockSpeed>,
        target_hz: u32,
        wait_prvdata_complete: bool,
    },
    ExternalSetClock {
        speed: Option<ClockSpeed>,
        target_hz: u32,
        wait_prvdata_complete: bool,
    },
    WaitGate {
        polls: u32,
        target_hz: u32,
    },
    ProgramDivider {
        target_hz: u32,
    },
    WaitDivider {
        polls: u32,
    },
    Enable,
    WaitEnable {
        polls: u32,
    },
}

const DWMMC_RESET_POLLS: u32 = host::DWMMC_HW_POLL_LIMIT;
const DWMMC_CLOCK_POLLS: u32 = host::DWMMC_HW_POLL_LIMIT;

impl DwMmc {
    pub(super) fn prepare_host2_bus_op(
        &self,
        op: sdmmc_host::BusOp,
    ) -> Result<BusRequestState, sdmmc_host::Error> {
        match op {
            sdmmc_host::BusOp::ResetAll => Ok(BusRequestState::ResetAll {
                state: DwMmcResetState::Start,
                restore_completion_irq: self.completion_irq_enabled(),
            }),
            sdmmc_host::BusOp::ResetCommandLine => Err(sdmmc_host::Error::Unsupported),
            sdmmc_host::BusOp::ResetDataLine => Ok(BusRequestState::ResetDataLine {
                started: false,
                polls: 0,
            }),
            sdmmc_host::BusOp::PowerOn => Ok(BusRequestState::PowerOn {
                state: DwMmcResetState::Start,
                restore_completion_irq: self.completion_irq_enabled(),
            }),
            sdmmc_host::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdmmc_host::BusOp::SetClock(speed) => {
                let target_hz = clock_hz_for_speed(speed);
                if target_hz == 0 {
                    return Err(sdmmc_host::Error::Unsupported);
                }
                Ok(BusRequestState::SetClock(DwMmcClockState::Start {
                    speed: Some(speed),
                    target_hz,
                    wait_prvdata_complete: true,
                }))
            }
            sdmmc_host::BusOp::SetClockHz(sdmmc_host::ClockHz(hz)) => {
                Ok(BusRequestState::SetClock(DwMmcClockState::Start {
                    speed: None,
                    target_hz: hz,
                    wait_prvdata_complete: true,
                }))
            }
            sdmmc_host::BusOp::SetBusWidth(width) => Ok(BusRequestState::SetBusWidth(width)),
            sdmmc_host::BusOp::SetSignalVoltage(voltage) => match volt_mask_for_signal(voltage) {
                Ok(_) => Ok(BusRequestState::SetSignalVoltage(voltage)),
                Err(err) => Err(map_protocol_error(err)),
            },
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
            BusRequestState::PowerOn {
                state,
                restore_completion_irq,
            } => self.advance_host2_power_on(state, *restore_completion_irq),
            BusRequestState::PowerOff => {
                self.regs.pwren().write(0);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.advance_host2_clock(clock),
            BusRequestState::SetBusWidth(width) => {
                self.set_card_type(*width);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => {
                self.set_signal_voltage(*voltage)
                    .map_err(map_protocol_error)?;
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
        }
    }

    fn advance_host2_reset_all(
        &mut self,
        state: &mut DwMmcResetState,
        restore_completion_irq: bool,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        match state {
            DwMmcResetState::Start => {
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
                *state = DwMmcResetState::WaitReset { polls: 0 };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcResetState::WaitReset { polls } => {
                let ctrl = self.regs.ctrl().read();
                if !ctrl.controller_reset() && !ctrl.fifo_reset() && !ctrl.dma_reset() {
                    self.regs.intmask().write(0);
                    self.clear_all_int_status();
                    self.irq.state.clear(u32::MAX);
                    if self.idmac_ring.is_some() || self.dma_poisoned {
                        if self.regs.status().read().dma_req() {
                            *state = DwMmcResetState::WaitDmaRequest { polls: 0 };
                        } else {
                            self.start_second_fifo_reset();
                            *state = DwMmcResetState::WaitSecondFifoReset { polls: 0 };
                        }
                        return Ok(sdmmc_host::RequestProgress::WaitingForIrq);
                    }
                    self.finish_host2_reset_all(restore_completion_irq);
                    return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
                }
                if *polls >= DWMMC_RESET_POLLS {
                    self.log_host2_timeout("reset-all");
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcResetState::WaitDmaRequest { polls } => {
                if !self.regs.status().read().dma_req() {
                    self.start_second_fifo_reset();
                    *state = DwMmcResetState::WaitSecondFifoReset { polls: 0 };
                    return Ok(sdmmc_host::RequestProgress::WaitingForIrq);
                }
                if *polls >= DWMMC_RESET_POLLS {
                    self.log_host2_timeout("reset-all-dma-request");
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcResetState::WaitSecondFifoReset { polls } => {
                if !self.regs.ctrl().read().fifo_reset() {
                    self.finish_host2_reset_all(restore_completion_irq);
                    return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
                }
                if *polls >= DWMMC_RESET_POLLS {
                    self.log_host2_timeout("reset-all-second-fifo");
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
        }
    }

    fn start_second_fifo_reset(&mut self) {
        self.regs.ctrl().update(|ctrl| ctrl.with_fifo_reset(true));
    }

    fn finish_host2_reset_all(&mut self, restore_completion_irq: bool) {
        self.regs.ctype().write(crate::regs::CType::new());
        self.regs.uhs().write(crate::regs::UHS::new());
        self.program_linux_init_baseline();
        if let Some(ring) = self.idmac_ring.as_mut() {
            ring.clear_after_reset();
        }
        self.dma_poisoned = false;
        if restore_completion_irq {
            self.enable_completion_irq();
        } else {
            self.completion_irq_enabled
                .store(false, core::sync::atomic::Ordering::Release);
        }
    }

    fn advance_host2_power_on(
        &mut self,
        state: &mut DwMmcResetState,
        restore_completion_irq: bool,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        if matches!(state, DwMmcResetState::Start) {
            self.regs.pwren().write(1);
        }
        self.advance_host2_reset_all(state, restore_completion_irq)
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
        if *polls >= DWMMC_RESET_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::DataRead,
            ))));
        }
        *polls += 1;
        Ok(sdmmc_host::RequestProgress::WaitingForIrq)
    }

    fn advance_host2_clock(
        &mut self,
        state: &mut DwMmcClockState,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        match state {
            DwMmcClockState::Start {
                speed,
                target_hz,
                wait_prvdata_complete,
            } => {
                if self.ext_clock.is_some() {
                    *state = DwMmcClockState::ExternalSetClock {
                        speed: *speed,
                        target_hz: *target_hz,
                        wait_prvdata_complete: *wait_prvdata_complete,
                    };
                    return Ok(sdmmc_host::RequestProgress::WaitingForIrq);
                }
                if let Some(speed) = *speed {
                    self.set_uhs_timing(speed);
                }
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.clksrc().write(0);
                self.start_update_clock(false, *wait_prvdata_complete);
                *state = DwMmcClockState::WaitGate {
                    polls: 0,
                    target_hz: *target_hz,
                };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcClockState::ExternalSetClock {
                speed,
                target_hz,
                wait_prvdata_complete,
            } => {
                let clock = self.ext_clock.take().ok_or(sdmmc_host::Error::Controller)?;
                let result = clock.set_clock(*target_hz);
                self.ext_clock = Some(clock);
                let bus_hz = result.map_err(map_protocol_error)?;
                self.set_reference_clock(bus_hz);
                if let Some(speed) = *speed {
                    self.set_uhs_timing(speed);
                }
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.clksrc().write(0);
                self.start_update_clock(false, *wait_prvdata_complete);
                *state = DwMmcClockState::WaitGate {
                    polls: 0,
                    target_hz: *target_hz,
                };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcClockState::WaitGate { polls, target_hz } => {
                if self.poll_update_clock_complete(polls)? {
                    *state = DwMmcClockState::ProgramDivider {
                        target_hz: *target_hz,
                    };
                }
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcClockState::ProgramDivider { target_hz } => {
                let div = dwmmc_clock_divisor(self.ref_clock_hz, *target_hz);
                self.regs
                    .clkdiv()
                    .write(crate::regs::ClkDiv::new().with_clk_divider0(div));
                self.start_update_clock(false, true);
                *state = DwMmcClockState::WaitDivider { polls: 0 };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcClockState::WaitDivider { polls } => {
                if self.poll_update_clock_complete(polls)? {
                    *state = DwMmcClockState::Enable;
                }
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcClockState::Enable => {
                self.regs
                    .clkena()
                    .write(crate::regs::ClkEna::new().with_cclk_enable(1));
                self.start_update_clock(false, true);
                *state = DwMmcClockState::WaitEnable { polls: 0 };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            DwMmcClockState::WaitEnable { polls } => {
                if self.poll_update_clock_complete(polls)? {
                    return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
                }
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
        }
    }

    fn start_update_clock(&self, voltage_switch: bool, wait_prvdata_complete: bool) {
        self.regs.cmd().write(
            crate::regs::Cmd::new()
                .with_start_cmd(true)
                .with_use_hold_reg(false)
                .with_wait_prvdata_complete(wait_prvdata_complete)
                .with_update_clock_registers_only(true)
                .with_volt_switch(voltage_switch),
        );
    }

    fn poll_update_clock_complete(&self, polls: &mut u32) -> Result<bool, sdmmc_host::Error> {
        if !self.regs.cmd().read().start_cmd() {
            return Ok(true);
        }
        if *polls >= DWMMC_CLOCK_POLLS {
            self.log_host2_timeout("clock-update");
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        *polls += 1;
        Ok(false)
    }

    fn log_host2_timeout(&self, op: &str) {
        warn!(
            "dwmmc-host2: {op} timeout ctrl={:#010x} cmd={:#010x} status={:#010x} \
             rintsts={:#010x} mintsts={:#010x} intmask={:#010x} clkena={:#010x} clksrc={:#010x} \
             clkdiv={:#010x} ctype={:#010x} pwren={:#010x} fifoth={:#010x} tmout={:#010x}",
            self.regs.ctrl().read().into_bits(),
            self.regs.cmd().read().into_bits(),
            self.regs.status().read().into_bits(),
            self.regs.rintsts().read().into_bits(),
            self.regs.mintsts().read(),
            self.regs.intmask().read(),
            self.regs.clkena().read().into_bits(),
            self.regs.clksrc().read(),
            self.regs.clkdiv().read().into_bits(),
            self.regs.ctype().read().into_bits(),
            self.regs.pwren().read(),
            self.regs.fifoth().read(),
            self.regs.tmout().read(),
        );
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
                self.reset_fifo().map_err(map_protocol_error)?;
            }
            BusRequestState::PowerOn { .. }
            | BusRequestState::PowerOff
            | BusRequestState::SetBusWidth(_) => {}
            BusRequestState::ResetDataLine { .. } => {}
        }
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.controller_data_complete = false;
        self.idmac_data_complete = false;
        self.command_state = command::CommandState::Idle;
        Ok(())
    }
}
