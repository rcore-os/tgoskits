//! Host2 controller state transitions.

use super::*;
use crate::timing::{clock_hz_for_speed, dwmmc_clock_divisor};

impl DwMmc {
    pub(super) fn physical_bus_idle(&self) -> bool {
        matches!(self.command_state, command::CommandState::Idle)
            && self.pending_data.is_none()
            && self.data_blocks_remaining == 0
            && self.host2_active_id.is_none()
    }

    pub(super) fn start_host2_request(&mut self) -> u64 {
        let id = self.host2_next_id;
        self.host2_next_id = self.host2_next_id.wrapping_add(1);
        self.host2_active_id = Some(id);
        id
    }

    pub(super) fn host2_owner(&self) -> usize {
        self.base_addr
    }

    pub(super) fn finish_host2_request(&mut self, id: u64) {
        if self.host2_active_id == Some(id) {
            self.host2_active_id = None;
        }
    }

    pub(super) fn finish_host2_bus_poll(
        &mut self,
        request: &mut BusRequest,
        progress: Result<sdio_host2::RequestPoll<()>, sdio_host2::Error>,
    ) -> sdio_host2::RequestPoll<()> {
        match progress {
            Ok(sdio_host2::RequestPoll::Pending) => sdio_host2::RequestPoll::Pending,
            Ok(sdio_host2::RequestPoll::Ready(Ok(()))) => {
                self.complete_host2_bus_request(request);
                sdio_host2::RequestPoll::Ready(Ok(()))
            }
            Ok(sdio_host2::RequestPoll::Ready(Err(error))) | Err(error) => {
                // A staged initializer owns recovery policy. Synchronously
                // resetting here would turn a deadline failure back into an
                // unbounded task-context poll loop.
                self.poison_dma();
                self.complete_host2_bus_request(request);
                sdio_host2::RequestPoll::Ready(Err(error))
            }
        }
    }

    pub(super) fn prepare_host2_bus_op(
        &self,
        op: sdio_host2::BusOp,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        match op {
            sdio_host2::BusOp::ResetAll => Ok(BusRequestState::ResetAll(DwMmcResetState::Start)),
            sdio_host2::BusOp::ResetCommandLine => Err(sdio_host2::Error::Unsupported),
            sdio_host2::BusOp::ResetDataLine => {
                Ok(BusRequestState::ResetDataLine(DwMmcFifoResetState::Start))
            }
            sdio_host2::BusOp::PowerOn => Ok(BusRequestState::PowerOn(DwMmcResetState::Start)),
            sdio_host2::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdio_host2::BusOp::SetClock(speed) => {
                let target_hz = clock_hz_for_speed(speed);
                if target_hz == 0 {
                    return Err(sdio_host2::Error::Unsupported);
                }
                Ok(BusRequestState::SetClock(DwMmcClockState::Start {
                    speed: Some(speed),
                    target_hz,
                    wait_prvdata_complete: true,
                }))
            }
            sdio_host2::BusOp::SetClockHz(sdio_host2::ClockHz(hz)) => {
                Ok(BusRequestState::SetClock(DwMmcClockState::Start {
                    speed: None,
                    target_hz: hz,
                    wait_prvdata_complete: true,
                }))
            }
            sdio_host2::BusOp::SetBusWidth(width) => match width {
                BusWidth::Bit1 | BusWidth::Bit4 | BusWidth::Bit8 => {
                    Ok(BusRequestState::SetBusWidth(width))
                }
                _ => Err(sdio_host2::Error::Unsupported),
            },
            sdio_host2::BusOp::SetSignalVoltage(voltage) => match volt_mask_for_signal(voltage) {
                Ok(_) => Ok(BusRequestState::SetSignalVoltage(voltage)),
                Err(err) => Err(map_protocol_error(err)),
            },
            sdio_host2::BusOp::ExecuteTuning { .. } => Err(sdio_host2::Error::Unsupported),
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    pub(super) fn poll_host2_bus_state_at(
        &mut self,
        state: &mut BusRequestState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            BusRequestState::ResetAll(reset) => self.poll_host2_reset_all_at(reset, now_ns),
            BusRequestState::ResetDataLine(reset) => self.poll_host2_fifo_reset_at(reset, now_ns),
            BusRequestState::PowerOn(reset) => self.poll_host2_power_on_at(reset, now_ns),
            BusRequestState::PowerOff => {
                self.regs.pwren().write(0);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.poll_host2_clock_at(clock, now_ns),
            BusRequestState::SetBusWidth(width) => {
                self.set_card_type(*width);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => {
                self.set_signal_voltage(*voltage)
                    .map_err(map_protocol_error)?;
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
        }
    }

    pub(super) fn poll_host2_reset_all_at(
        &mut self,
        state: &mut DwMmcResetState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
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
                *state = DwMmcResetState::WaitReset {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcResetState::WaitReset { wait } => {
                let ctrl = self.regs.ctrl().read();
                if !ctrl.controller_reset() && !ctrl.fifo_reset() && !ctrl.dma_reset() {
                    self.regs.intmask().write(0);
                    self.clear_all_int_status();
                    self.clear_all_idmac_status();
                    self.clear_task_irq_evidence();
                    self.regs.ctype().write(crate::regs::CType::new());
                    self.regs.uhs().write(crate::regs::UHS::new());
                    self.program_linux_init_baseline();
                    if self.completion_irq_enabled() {
                        self.enable_completion_irq();
                    }
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                if wait.expired(now_ns) {
                    self.log_host2_timeout("reset-all");
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                wait.defer(now_ns);
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    pub(super) fn poll_host2_power_on_at(
        &mut self,
        state: &mut DwMmcResetState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        if matches!(state, DwMmcResetState::Start) {
            self.regs.pwren().write(1);
        }
        self.poll_host2_reset_all_at(state, now_ns)
    }

    pub(super) fn poll_host2_fifo_reset_at(
        &mut self,
        state: &mut DwMmcFifoResetState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            DwMmcFifoResetState::Start => {
                self.regs.ctrl().update(|r| r.with_fifo_reset(true));
                *state = DwMmcFifoResetState::WaitReset {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcFifoResetState::WaitReset { wait } => {
                if !self.regs.ctrl().read().fifo_reset() {
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                if wait.expired(now_ns) {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::DataRead,
                    ))));
                }
                wait.defer(now_ns);
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    pub(super) fn poll_host2_clock_at(
        &mut self,
        state: &mut DwMmcClockState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
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
                    return Ok(sdio_host2::RequestPoll::Pending);
                }
                if let Some(speed) = *speed {
                    self.set_uhs_timing(speed);
                }
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.clksrc().write(0);
                self.start_update_clock(false, *wait_prvdata_complete);
                *state = DwMmcClockState::WaitGate {
                    wait: Host2TimedWait::start(now_ns),
                    target_hz: *target_hz,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::ExternalSetClock {
                speed,
                target_hz,
                wait_prvdata_complete,
            } => {
                let clock = self.ext_clock.take().ok_or(sdio_host2::Error::Controller)?;
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
                    wait: Host2TimedWait::start(now_ns),
                    target_hz: *target_hz,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::WaitGate { wait, target_hz } => {
                if self.poll_update_clock_complete_at(wait, now_ns)? {
                    *state = DwMmcClockState::ProgramDivider {
                        target_hz: *target_hz,
                    };
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::ProgramDivider { target_hz } => {
                let div = dwmmc_clock_divisor(self.ref_clock_hz, *target_hz);
                self.regs
                    .clkdiv()
                    .write(crate::regs::ClkDiv::new().with_clk_divider0(div));
                self.start_update_clock(false, true);
                *state = DwMmcClockState::WaitDivider {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::WaitDivider { wait } => {
                if self.poll_update_clock_complete_at(wait, now_ns)? {
                    *state = DwMmcClockState::Enable;
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::Enable => {
                self.regs
                    .clkena()
                    .write(crate::regs::ClkEna::new().with_cclk_enable(1));
                self.start_update_clock(false, true);
                *state = DwMmcClockState::WaitEnable {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::WaitEnable { wait } => {
                if self.poll_update_clock_complete_at(wait, now_ns)? {
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    pub(super) fn start_update_clock(&self, voltage_switch: bool, wait_prvdata_complete: bool) {
        self.regs.cmd().write(
            crate::regs::Cmd::new()
                .with_start_cmd(true)
                .with_use_hold_reg(false)
                .with_wait_prvdata_complete(wait_prvdata_complete)
                .with_update_clock_registers_only(true)
                .with_volt_switch(voltage_switch),
        );
    }

    pub(super) fn poll_update_clock_complete_at(
        &self,
        wait: &mut Host2TimedWait,
        now_ns: u64,
    ) -> Result<bool, sdio_host2::Error> {
        if !self.regs.cmd().read().start_cmd() {
            return Ok(true);
        }
        if wait.expired(now_ns) {
            self.log_host2_timeout("clock-update");
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        wait.defer(now_ns);
        Ok(false)
    }

    pub(super) fn log_host2_timeout(&self, op: &str) {
        warn!(
            "dwmmc-host2: {op} timeout ctrl={:#010x} cmd={:#010x} status={:#010x} \
             cached_irq={:#010x} cached_idmac={:#010x} intmask={:#010x} clkena={:#010x} \
             clksrc={:#010x} clkdiv={:#010x} ctype={:#010x} pwren={:#010x} fifoth={:#010x} \
             tmout={:#010x}",
            self.regs.ctrl().read().into_bits(),
            self.regs.cmd().read().into_bits(),
            self.regs.status().read().into_bits(),
            self.irq.state.pending(),
            self.irq.state.pending_idmac(),
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

    pub(super) fn check_host2_transaction_request(
        &self,
        request: &TransactionRequest<'_>,
    ) -> Result<(), sdio_host2::PollRequestError> {
        if request.done {
            return Err(sdio_host2::PollRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::PollRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdio_host2::PollRequestError::StaleGeneration);
        }
        Ok(())
    }

    pub(super) fn check_host2_bus_request(
        &self,
        request: &BusRequest,
    ) -> Result<(), sdio_host2::PollRequestError> {
        if request.done {
            return Err(sdio_host2::PollRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::PollRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdio_host2::PollRequestError::StaleGeneration);
        }
        Ok(())
    }

    pub(super) fn complete_host2_transaction_request(
        &mut self,
        request: &mut TransactionRequest<'_>,
    ) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    pub(super) fn complete_host2_bus_request(&mut self, request: &mut BusRequest) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    pub(super) fn abort_host2_bus_state(
        &mut self,
        _state: &mut BusRequestState,
    ) -> Result<(), sdio_host2::Error> {
        if !self.recovery_quiesced {
            return Err(sdio_host2::Error::Busy);
        }
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.command_state = command::CommandState::Idle;
        Ok(())
    }

    pub(super) fn abort_host2_transaction_request(
        &mut self,
        request: &mut TransactionRequest<'_>,
    ) -> Result<(), sdio_host2::Error> {
        if !self.recovery_quiesced {
            return Err(sdio_host2::Error::Busy);
        }
        let result = if let Some(data) = request.data.as_mut() {
            if let Some(active) = data.request.take() {
                let id = active.id();
                let mut pending = Some(active);
                self.reclaim_block_request_after_quiesce(&mut pending, id, &mut data.slot)
                    .map_err(map_protocol_error)
            } else {
                Ok(())
            }
        } else {
            self.abort_command().map_err(map_protocol_error)
        };
        if !matches!(result, Err(sdio_host2::Error::Busy)) {
            request.done = true;
            self.finish_host2_request(request.id);
        }
        result
    }
}

pub(super) fn map_protocol_error(err: Error) -> sdio_host2::Error {
    match err {
        Error::Timeout(_) => sdio_host2::Error::Timeout,
        Error::Crc(_) => sdio_host2::Error::Crc,
        Error::NoCard => sdio_host2::Error::NoCard,
        Error::Busy => sdio_host2::Error::Busy,
        Error::UnsupportedCommand => sdio_host2::Error::Unsupported,
        Error::Misaligned => sdio_host2::Error::Misaligned,
        Error::InvalidArgument => sdio_host2::Error::InvalidArgument,
        Error::BusError(_) => sdio_host2::Error::Bus,
        Error::ReadError(_) | Error::WriteError(_) | Error::BadResponse(_) => {
            sdio_host2::Error::Bus
        }
        Error::CardError(_) | Error::CardLocked => sdio_host2::Error::Controller,
        _ => sdio_host2::Error::Controller,
    }
}
