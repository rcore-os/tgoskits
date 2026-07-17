use super::*;

impl PhytiumMci {
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
                // Recovery belongs to the staged initializer. A synchronous
                // reset here would reintroduce call-count polling after an
                // absolute deadline has already fired.
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
            sdio_host2::BusOp::ResetAll => Ok(BusRequestState::ResetAll(PhytiumResetState::Start)),
            sdio_host2::BusOp::ResetCommandLine => Err(sdio_host2::Error::Unsupported),
            sdio_host2::BusOp::ResetDataLine => {
                Ok(BusRequestState::ResetDataLine(PhytiumFifoResetState::Start))
            }
            sdio_host2::BusOp::PowerOn => Ok(BusRequestState::PowerOn),
            sdio_host2::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdio_host2::BusOp::SetClock(speed) => {
                let timing =
                    timing::TimingTable::for_host_speed(speed).map_err(map_protocol_error)?;
                Ok(BusRequestState::SetClock(PhytiumClockState::Start {
                    timing,
                }))
            }
            sdio_host2::BusOp::SetClockHz(_) => Err(sdio_host2::Error::Unsupported),
            sdio_host2::BusOp::SetBusWidth(width) => match width {
                BusWidth::Bit1 | BusWidth::Bit4 | BusWidth::Bit8 => {
                    Ok(BusRequestState::SetBusWidth(width))
                }
                _ => Err(sdio_host2::Error::Unsupported),
            },
            sdio_host2::BusOp::SetSignalVoltage(voltage) => {
                uhs_bits_after_voltage(self.regs.uhs().read(), voltage)
                    .map_err(map_protocol_error)?;
                Ok(BusRequestState::SetSignalVoltage(
                    PhytiumVoltageState::Start(voltage),
                ))
            }
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
            BusRequestState::PowerOn => {
                self.regs.pwren().write(1);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::PowerOff => {
                self.regs.pwren().write(0);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.poll_host2_clock_at(clock, now_ns),
            BusRequestState::SetBusWidth(width) => {
                PhytiumMci::set_bus_width(self, *width);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => {
                self.poll_host2_voltage_at(voltage, now_ns)
            }
        }
    }

    pub(super) fn poll_host2_reset_all_at(
        &mut self,
        state: &mut PhytiumResetState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
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
                *state = PhytiumResetState::WaitReset {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumResetState::WaitReset { wait } => {
                let ctrl = self.regs.ctrl().read();
                if !ctrl.controller_reset() && !ctrl.fifo_reset() && !ctrl.dma_reset() {
                    let irq = self.irq.clone();
                    let Some(_register_owner) = irq.state.try_begin_task_update() else {
                        if wait.expired(now_ns) {
                            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                                Phase::Init,
                            ))));
                        }
                        wait.defer(now_ns);
                        return Ok(sdio_host2::RequestPoll::Pending);
                    };
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
                    if self.completion_irq_enabled() {
                        self.enable_completion_irq();
                    }
                    *state = PhytiumResetState::InitClock(PhytiumClockState::Start {
                        timing: timing::TimingTable::for_host_speed(ClockSpeed::Identification)
                            .map_err(map_protocol_error)?,
                    });
                    return Ok(sdio_host2::RequestPoll::Pending);
                }
                if wait.expired(now_ns) {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                wait.defer(now_ns);
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumResetState::InitClock(clock) => self.poll_host2_clock_at(clock, now_ns),
        }
    }

    pub(super) fn poll_host2_fifo_reset_at(
        &mut self,
        state: &mut PhytiumFifoResetState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            PhytiumFifoResetState::Start => {
                self.regs.ctrl().update(|r| r.with_fifo_reset(true));
                *state = PhytiumFifoResetState::WaitReset {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumFifoResetState::WaitReset { wait } => {
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
        state: &mut PhytiumClockState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            PhytiumClockState::Start { timing } => {
                self.use_hold_reg = timing.use_hold;
                self.write_ext_reg(crate::regs::CLK_SRC_OFFSET, 0);
                self.write_ext_reg(crate::regs::CLK_SRC_OFFSET, timing.clk_src);
                *state = PhytiumClockState::WaitExternalClock {
                    wait: Host2TimedWait::start(now_ns),
                    timing: *timing,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::WaitExternalClock { wait, timing } => {
                if self.regs.cksts().read().ready() {
                    self.regs.clkena().write(crate::regs::ClkEna::new());
                    self.start_update_clock(false);
                    *state = PhytiumClockState::WaitDisable {
                        wait: Host2TimedWait::start(now_ns),
                        timing: *timing,
                    };
                    return Ok(sdio_host2::RequestPoll::Pending);
                }
                if wait.expired(now_ns) {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                wait.defer(now_ns);
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::WaitDisable { wait, timing } => {
                if self.poll_update_clock_complete_at(wait, now_ns)? {
                    *state = PhytiumClockState::ProgramDivider { timing: *timing };
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::ProgramDivider { timing } => {
                self.regs.clkdiv().write(timing.clk_div);
                self.regs
                    .clkena()
                    .write(crate::regs::ClkEna::new().with_cclk_enable(1));
                self.start_update_clock(false);
                *state = PhytiumClockState::WaitEnable {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::WaitEnable { wait } => {
                if self.poll_update_clock_complete_at(wait, now_ns)? {
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    pub(super) fn poll_host2_voltage_at(
        &mut self,
        state: &mut PhytiumVoltageState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            PhytiumVoltageState::Start(voltage) => {
                let next = uhs_bits_after_voltage(self.regs.uhs().read(), *voltage)
                    .map_err(map_protocol_error)?;
                self.regs.uhs().write(next);
                self.start_update_clock(matches!(*voltage, SignalVoltage::V180));
                *state = PhytiumVoltageState::WaitUpdate {
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumVoltageState::WaitUpdate { wait } => {
                if self.poll_update_clock_complete_at(wait, now_ns)? {
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    pub(super) fn start_update_clock(&self, voltage_switch: bool) {
        self.regs.cmd().write(
            crate::regs::Cmd::new()
                .with_start_cmd(true)
                .with_wait_prvdata_complete(true)
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
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        wait.defer(now_ns);
        Ok(false)
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
