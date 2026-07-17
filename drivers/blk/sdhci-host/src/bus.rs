//! Bounded host bus-operation state machines.

use crate::{
    transfer::{map_protocol_error, sdhci_clock_divisor},
    *,
};

impl Sdhci {
    pub(crate) fn physical_bus_idle(&self) -> bool {
        matches!(self.command_state, command::CommandState::Idle)
            && self.pending_data.is_none()
            && self.host2_active_id.is_none()
    }

    pub(crate) fn start_host2_request(&mut self) -> u64 {
        let id = self.host2_next_id;
        self.host2_next_id = self.host2_next_id.wrapping_add(1);
        self.host2_active_id = Some(id);
        id
    }

    pub(crate) fn host2_owner(&self) -> usize {
        self.base_addr
    }

    pub(crate) fn finish_host2_request(&mut self, id: u64) {
        if self.host2_active_id == Some(id) {
            self.host2_active_id = None;
        }
    }

    pub(crate) fn finish_host2_bus_poll(
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
                // Staged initialization owns recovery. Running the legacy
                // reset helper here would turn an absolute timeout back into
                // task-context register polling.
                self.cancel_failed_host2_bus_state(&mut request.state);
                self.poison_dma();
                self.complete_host2_bus_request(request);
                sdio_host2::RequestPoll::Ready(Err(error))
            }
        }
    }

    pub(crate) fn prepare_host2_bus_op(
        &self,
        op: sdio_host2::BusOp,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        match op {
            sdio_host2::BusOp::ResetAll => Ok(BusRequestState::Reset {
                mask: RESET_ALL,
                phase: Phase::Init,
                was_irq_enabled: self.completion_irq_enabled(),
                state: SdhciResetState::Start,
            }),
            sdio_host2::BusOp::ResetCommandLine => Ok(BusRequestState::Reset {
                mask: RESET_CMD,
                phase: Phase::CommandSend,
                was_irq_enabled: self.completion_irq_enabled(),
                state: SdhciResetState::Start,
            }),
            sdio_host2::BusOp::ResetDataLine => Ok(BusRequestState::Reset {
                mask: RESET_DAT,
                phase: Phase::DataRead,
                was_irq_enabled: self.completion_irq_enabled(),
                state: SdhciResetState::Start,
            }),
            sdio_host2::BusOp::PowerOn => Ok(BusRequestState::PowerOn),
            sdio_host2::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdio_host2::BusOp::SetClock(speed) => self.prepare_host2_clock(speed),
            sdio_host2::BusOp::SetClockHz(sdio_host2::ClockHz(hz)) => {
                if self.ext_clock.is_none() && self.base_clock_hz() == 0 {
                    return Err(sdio_host2::Error::Controller);
                }
                Ok(BusRequestState::SetClock(SdhciClockState::Start {
                    target_hz: hz,
                    uhs_mode: None,
                    high_speed: None,
                }))
            }
            sdio_host2::BusOp::SetBusWidth(width) => match width {
                BusWidth::Bit1 | BusWidth::Bit4 | BusWidth::Bit8 => {
                    Ok(BusRequestState::SetBusWidth(width))
                }
                _ => Err(sdio_host2::Error::Unsupported),
            },
            sdio_host2::BusOp::SetSignalVoltage(voltage) => self.prepare_host2_voltage(voltage),
            sdio_host2::BusOp::ExecuteTuning {
                command,
                block_size,
            } => self.prepare_host2_tuning(command, block_size),
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    fn prepare_host2_clock(&self, speed: ClockSpeed) -> Result<BusRequestState, sdio_host2::Error> {
        let (target_hz, uhs_mode) = match speed {
            ClockSpeed::Identification => (400_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::Default | ClockSpeed::Sdr12 => (25_000_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => (50_000_000, HOST_CTRL2_UHS_SDR25),
            ClockSpeed::Sdr50 => (50_000_000, HOST_CTRL2_UHS_SDR50),
            ClockSpeed::Ddr50 => (50_000_000, HOST_CTRL2_UHS_DDR50),
            ClockSpeed::Sdr104 => (104_000_000, HOST_CTRL2_UHS_SDR104),
            ClockSpeed::Hs200 => (200_000_000, HOST_CTRL2_UHS_SDR104),
            _ => return Err(sdio_host2::Error::Unsupported),
        };
        if self.ext_clock.is_none() && self.base_clock_hz() == 0 {
            return Err(sdio_host2::Error::Controller);
        }
        let high_speed = !matches!(
            speed,
            ClockSpeed::Identification | ClockSpeed::Default | ClockSpeed::Sdr12
        );
        Ok(BusRequestState::SetClock(SdhciClockState::Start {
            target_hz,
            uhs_mode: Some(uhs_mode),
            high_speed: Some(high_speed),
        }))
    }

    fn prepare_host2_voltage(
        &self,
        voltage: SignalVoltage,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        if matches!(voltage, SignalVoltage::V180) && !self.support_1v8 {
            return Err(sdio_host2::Error::Unsupported);
        }
        match voltage {
            SignalVoltage::V330 | SignalVoltage::V180 => Ok(BusRequestState::SetSignalVoltage(
                SdhciVoltageState::DisableClock(voltage),
            )),
            SignalVoltage::V120 => Err(sdio_host2::Error::Unsupported),
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    fn prepare_host2_tuning(
        &self,
        command: sdio_host2::Command,
        block_size: core::num::NonZeroU16,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        if command.index != 19 && command.index != 21 {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        let expected =
            if command.index == 21 && self.read_u8(REG_HOST_CONTROL1) & HOST_CTRL1_8BIT != 0 {
                sdmmc_protocol::cmd::MMC_TUNING_BLOCK_SIZE_8BIT
            } else {
                sdmmc_protocol::cmd::SD_TUNING_BLOCK_SIZE
            };
        if u32::from(block_size.get()) != expected {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        Ok(BusRequestState::ExecuteTuning(SdhciTuningState::Start {
            cmd_index: command.index,
            block_size: block_size.get(),
        }))
    }

    pub(crate) fn poll_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        // The compatibility trait has no time input. It may observe hardware
        // progress, but only `SdioHost2Timed` can enforce deadlines.
        self.poll_host2_bus_state_at(state, 0)
    }

    pub(crate) fn poll_host2_bus_state_at(
        &mut self,
        state: &mut BusRequestState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            BusRequestState::Reset {
                mask,
                phase,
                was_irq_enabled,
                state,
            } => self.poll_host2_reset_at(*mask, *phase, *was_irq_enabled, state, now_ns),
            BusRequestState::PowerOn => {
                self.set_power(POWER_330);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::PowerOff => {
                self.write_u8(REG_POWER_CONTROL, 0);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.poll_host2_clock_at(clock, now_ns),
            BusRequestState::SetBusWidth(width) => {
                self.apply_bus_width(*width).map_err(map_protocol_error)?;
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => {
                self.poll_host2_voltage_at(voltage, now_ns)
            }
            BusRequestState::ExecuteTuning(tuning) => self.poll_host2_tuning_at(tuning, now_ns),
        }
    }

    fn poll_host2_reset_at(
        &mut self,
        mask: u8,
        phase: Phase,
        was_irq_enabled: bool,
        state: &mut SdhciResetState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match *state {
            SdhciResetState::Start => {
                let hook_progress = if mask == RESET_ALL {
                    self.begin_before_reset_all_hook(now_ns)
                        .map_err(map_protocol_error)?
                } else {
                    ResetHookPoll::Ready
                };
                match hook_progress {
                    ResetHookPoll::Ready => {
                        self.write_u8(REG_SOFTWARE_RESET, mask);
                        *state = SdhciResetState::WaitController {
                            wait: Host2TimedWait::start(now_ns),
                        };
                        Ok(sdio_host2::RequestPoll::Pending)
                    }
                    ResetHookPoll::Pending { wake_at_ns } => {
                        *state = SdhciResetState::WaitHook { wake_at_ns };
                        Ok(sdio_host2::RequestPoll::Pending)
                    }
                }
            }
            SdhciResetState::WaitHook { wake_at_ns } => {
                if now_ns < wake_at_ns {
                    return Ok(sdio_host2::RequestPoll::Pending);
                }
                match self
                    .poll_before_reset_all_hook(now_ns)
                    .map_err(map_protocol_error)?
                {
                    ResetHookPoll::Ready => {
                        self.write_u8(REG_SOFTWARE_RESET, mask);
                        *state = SdhciResetState::WaitController {
                            wait: Host2TimedWait::start(now_ns),
                        };
                        Ok(sdio_host2::RequestPoll::Pending)
                    }
                    ResetHookPoll::Pending { wake_at_ns } => {
                        *state = SdhciResetState::WaitHook { wake_at_ns };
                        Ok(sdio_host2::RequestPoll::Pending)
                    }
                }
            }
            SdhciResetState::WaitController { mut wait } => {
                if self.read_u8(REG_SOFTWARE_RESET) & mask == 0 {
                    if mask == RESET_ALL {
                        self.call_after_reset_hook().map_err(map_protocol_error)?;
                        self.restore_completion_irq_after_reset(was_irq_enabled);
                    }
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                if wait.expired(now_ns) {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(phase))));
                }
                wait.defer(now_ns);
                *state = SdhciResetState::WaitController { wait };
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    fn poll_host2_clock_at(
        &mut self,
        state: &mut SdhciClockState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match *state {
            SdhciClockState::Start {
                target_hz,
                uhs_mode,
                high_speed,
            } => {
                if let Some(mode) = uhs_mode {
                    let ctrl2 =
                        (self.read_u16(REG_HOST_CONTROL2) & !HOST_CTRL2_UHS_MODE_MASK) | mode;
                    self.write_u16(REG_HOST_CONTROL2, ctrl2);
                }
                if let Some(enabled) = high_speed
                    && self.controls_high_speed_bit()
                {
                    let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
                    if enabled {
                        ctrl |= HOST_CTRL1_HIGH_SPEED;
                    } else {
                        ctrl &= !HOST_CTRL1_HIGH_SPEED;
                    }
                    self.write_u8(REG_HOST_CONTROL1, ctrl);
                }
                if self.ext_clock.is_some() {
                    self.disable_sd_clock();
                    *state = SdhciClockState::ExternalSetClock { target_hz };
                } else {
                    self.start_internal_clock(target_hz)?;
                    *state = SdhciClockState::InternalWaitStable {
                        target_hz,
                        wait: Host2TimedWait::start(now_ns),
                    };
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciClockState::ExternalSetClock { target_hz } => {
                let clock = self
                    .ext_clock
                    .as_ref()
                    .ok_or(sdio_host2::Error::Controller)?;
                let effective_hz = clock.effective_clock_hz(target_hz);
                clock.set_clock(effective_hz).map_err(map_protocol_error)?;
                *state = SdhciClockState::ExternalPrepareHost {
                    target_hz: effective_hz,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciClockState::ExternalPrepareHost { target_hz } => {
                let clock = self.ext_clock.take().ok_or(sdio_host2::Error::Controller)?;
                let result = clock.prepare_host_clock(self, target_hz);
                self.ext_clock = Some(clock);
                result.map_err(map_protocol_error)?;
                *state = SdhciClockState::ExternalStart { target_hz };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciClockState::ExternalStart { target_hz } => {
                self.start_passthrough_clock(target_hz);
                *state = SdhciClockState::ExternalEnable {
                    target_hz,
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciClockState::ExternalEnable {
                target_hz,
                ref mut wait,
            }
            | SdhciClockState::InternalWaitStable {
                target_hz,
                ref mut wait,
            } => self.poll_clock_stable_at(wait, now_ns, target_hz),
        }
    }

    fn start_internal_clock(&mut self, target_hz: u32) -> Result<(), sdio_host2::Error> {
        self.write_u16(REG_CLOCK_CONTROL, 0);
        if target_hz == 0 {
            return Ok(());
        }
        let base_clock_hz = self.base_clock_hz();
        if base_clock_hz == 0 {
            return Err(sdio_host2::Error::Controller);
        }
        let div = sdhci_clock_divisor(base_clock_hz, target_hz);
        let clk_ctrl = ((div & 0xFF) << 8) | ((div & 0x300) >> 2) | CLOCK_INTERNAL_ENABLE;
        self.write_u16(REG_CLOCK_CONTROL, clk_ctrl);
        Ok(())
    }

    fn poll_clock_stable_at(
        &mut self,
        wait: &mut Host2TimedWait,
        now_ns: u64,
        target_hz: u32,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        if clock & CLOCK_INTERNAL_ENABLE == 0 {
            self.bus_clock_hz = 0;
            return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
        }
        if clock & CLOCK_INTERNAL_STABLE != 0 {
            self.write_u16(REG_CLOCK_CONTROL, clock | CLOCK_SD_ENABLE);
            self.bus_clock_hz = target_hz;
            return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
        }
        if wait.expired(now_ns) {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        wait.defer(now_ns);
        Ok(sdio_host2::RequestPoll::Pending)
    }

    fn poll_host2_voltage_at(
        &mut self,
        state: &mut SdhciVoltageState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match *state {
            SdhciVoltageState::DisableClock(voltage) => {
                self.disable_sd_clock();
                *state = SdhciVoltageState::SwitchControllerAndRail(voltage);
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::SwitchControllerAndRail(voltage) => {
                if matches!(voltage, SignalVoltage::V180) && !self.dat_3_0_lines_low() {
                    self.rollback_host2_voltage();
                    return Ok(sdio_host2::RequestPoll::Ready(Err(
                        sdio_host2::Error::Controller,
                    )));
                }
                let mut ctrl2 = self.read_u16(REG_HOST_CONTROL2);
                match voltage {
                    SignalVoltage::V330 => {
                        ctrl2 &= !HOST_CTRL2_1V8_SIGNALING;
                        self.set_power(POWER_330);
                    }
                    SignalVoltage::V180 => {
                        ctrl2 |= HOST_CTRL2_1V8_SIGNALING;
                        self.set_power(POWER_180);
                    }
                    SignalVoltage::V120 => return Err(sdio_host2::Error::Unsupported),
                    _ => return Err(sdio_host2::Error::Unsupported),
                }
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
                *state = SdhciVoltageState::WaitVsw {
                    voltage,
                    wake_at_ns: now_ns.saturating_add(SDHCI_VOLTAGE_SWITCH_DELAY_NS),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::WaitVsw {
                voltage,
                wake_at_ns,
            } => {
                if now_ns >= wake_at_ns {
                    *state = SdhciVoltageState::EnableClock(voltage);
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::EnableClock(voltage) => {
                let cur = self.read_u16(REG_CLOCK_CONTROL);
                self.write_u16(REG_CLOCK_CONTROL, cur | CLOCK_SD_ENABLE);
                *state = SdhciVoltageState::VerifyDatLines(voltage);
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::VerifyDatLines(voltage) => {
                if matches!(voltage, SignalVoltage::V180) && !self.dat_3_0_lines_high() {
                    self.rollback_host2_voltage();
                    return Ok(sdio_host2::RequestPoll::Ready(Err(
                        sdio_host2::Error::Controller,
                    )));
                }
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
        }
    }

    fn poll_host2_tuning_at(
        &mut self,
        state: &mut SdhciTuningState,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match *state {
            SdhciTuningState::Start {
                cmd_index,
                block_size,
            } => {
                self.write_u16(REG_BLOCK_SIZE, block_size & 0x0FFF);
                self.write_u16(REG_BLOCK_COUNT, 1);
                self.write_u8(REG_TIMEOUT_CONTROL, 0x0E);
                self.write_u16(
                    REG_TRANSFER_MODE,
                    XFER_MODE_BLOCK_COUNT_ENABLE | XFER_MODE_READ,
                );
                let ctrl2 = self.read_u16(REG_HOST_CONTROL2) | HOST_CTRL2_EXECUTE_TUNING;
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
                *state = SdhciTuningState::Wait {
                    cmd_index,
                    wait: Host2TimedWait::start(now_ns),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciTuningState::Wait {
                cmd_index,
                ref mut wait,
            } => {
                let status = self.read_u16(REG_HOST_CONTROL2);
                if status & HOST_CTRL2_EXECUTE_TUNING == 0 {
                    if status & HOST_CTRL2_SAMPLING_CLOCK_SELECT != 0 {
                        return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                    }
                    return Err(map_protocol_error(Error::BadResponse(
                        ErrorContext::for_cmd(Phase::Init, cmd_index),
                    )));
                }
                if wait.expired(now_ns) {
                    self.write_u16(REG_HOST_CONTROL2, status & !HOST_CTRL2_EXECUTE_TUNING);
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::for_cmd(
                        Phase::Init,
                        cmd_index,
                    ))));
                }
                wait.defer(now_ns);
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    pub(crate) fn abort_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<(), sdio_host2::Error> {
        if matches!(
            state,
            BusRequestState::Reset {
                state: SdhciResetState::WaitHook { .. },
                ..
            }
        ) {
            self.cancel_before_reset_all_hook()
                .map_err(map_protocol_error)?;
        }
        if !self.recovery_quiesced {
            return Err(sdio_host2::Error::Busy);
        }
        self.pending_data = None;
        self.command_state = command::CommandState::Idle;
        Ok(())
    }

    fn cancel_failed_host2_bus_state(&mut self, state: &mut BusRequestState) {
        match state {
            BusRequestState::Reset {
                state: SdhciResetState::WaitHook { .. },
                ..
            } => {
                let _ = self.cancel_before_reset_all_hook();
            }
            BusRequestState::SetSignalVoltage(_) => self.rollback_host2_voltage(),
            BusRequestState::ExecuteTuning(SdhciTuningState::Wait { .. }) => {
                let ctrl2 = self.read_u16(REG_HOST_CONTROL2) & !HOST_CTRL2_EXECUTE_TUNING;
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
            }
            _ => {}
        }
    }

    pub(crate) fn restore_completion_irq_after_reset(&mut self, was_irq_enabled: bool) {
        if self.runtime_irq_status_owned() {
            self.write_u16(REG_NORMAL_INT_STATUS_ENABLE, NORMAL_INT_CLEAR_ALL);
            self.write_u16(REG_ERROR_INT_STATUS_ENABLE, ERROR_INT_CLEAR_ALL);
            if was_irq_enabled {
                self.enable_completion_irq();
            } else {
                self.disable_completion_irq();
            }
            return;
        }
        self.enter_initialization_status_mode();
    }

    fn rollback_host2_voltage(&mut self) {
        self.disable_sd_clock();
        let ctrl2 = self.read_u16(REG_HOST_CONTROL2) & !HOST_CTRL2_1V8_SIGNALING;
        self.write_u16(REG_HOST_CONTROL2, ctrl2);
        self.set_power(POWER_330);
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        self.write_u16(REG_CLOCK_CONTROL, clock | CLOCK_SD_ENABLE);
    }

    fn dat_3_0_lines_high(&self) -> bool {
        self.read_u32(REG_PRESENT_STATE) & PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL
            == PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL
    }

    fn dat_3_0_lines_low(&self) -> bool {
        self.read_u32(REG_PRESENT_STATE) & PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL == 0
    }

    pub(crate) fn apply_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
        ctrl &= !(HOST_CTRL1_4BIT | HOST_CTRL1_8BIT);
        match width {
            BusWidth::Bit1 => {}
            BusWidth::Bit4 => ctrl |= HOST_CTRL1_4BIT,
            BusWidth::Bit8 => ctrl |= HOST_CTRL1_8BIT,
            _ => return Err(Error::UnsupportedCommand),
        }
        self.write_u8(REG_HOST_CONTROL1, ctrl);
        Ok(())
    }

    pub(crate) fn check_host2_transaction_request(
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

    pub(crate) fn check_host2_bus_request(
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

    pub(crate) fn complete_host2_transaction_request(
        &mut self,
        request: &mut TransactionRequest<'_>,
    ) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    fn complete_host2_bus_request(&mut self, request: &mut BusRequest) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    pub(crate) fn abort_host2_transaction_request(
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
