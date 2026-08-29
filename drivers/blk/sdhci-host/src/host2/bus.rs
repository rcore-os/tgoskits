use super::*;

impl Sdhci {
    pub(super) fn prepare_host2_bus_op(
        &self,
        op: sdmmc_host::BusOp,
    ) -> Result<BusRequestState, sdmmc_host::Error> {
        match op {
            sdmmc_host::BusOp::ResetAll => Ok(BusRequestState::Reset {
                mask: RESET_ALL,
                phase: Phase::Init,
                was_irq_enabled: self.completion_irq_enabled(),
                started: false,
                polls: 0,
            }),
            sdmmc_host::BusOp::ResetCommandLine => Ok(BusRequestState::Reset {
                mask: RESET_CMD,
                phase: Phase::CommandSend,
                was_irq_enabled: self.completion_irq_enabled(),
                started: false,
                polls: 0,
            }),
            sdmmc_host::BusOp::ResetDataLine => Ok(BusRequestState::Reset {
                mask: RESET_DAT,
                phase: Phase::DataRead,
                was_irq_enabled: self.completion_irq_enabled(),
                started: false,
                polls: 0,
            }),
            sdmmc_host::BusOp::PowerOn => Ok(BusRequestState::PowerOn),
            sdmmc_host::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdmmc_host::BusOp::SetClock(speed) => self.prepare_host2_clock(speed),
            sdmmc_host::BusOp::SetClockHz(sdmmc_host::ClockHz(hz)) => {
                if self.ext_clock.is_none() && self.base_clock_hz() == 0 {
                    return Err(sdmmc_host::Error::Controller);
                }
                Ok(BusRequestState::SetClock(SdhciClockState::Start {
                    target_hz: hz,
                    uhs_mode: None,
                    high_speed: None,
                }))
            }
            sdmmc_host::BusOp::SetBusWidth(width) => Ok(BusRequestState::SetBusWidth(width)),
            sdmmc_host::BusOp::SetSignalVoltage(voltage) => self.prepare_host2_voltage(voltage),
            sdmmc_host::BusOp::ExecuteTuning {
                command,
                block_size,
            } => self.prepare_host2_tuning(command, block_size),
            _ => Err(sdmmc_host::Error::Unsupported),
        }
    }

    fn prepare_host2_clock(&self, speed: ClockSpeed) -> Result<BusRequestState, sdmmc_host::Error> {
        let (target_hz, uhs_mode) = match speed {
            ClockSpeed::Identification => (400_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::Default | ClockSpeed::Sdr12 => (25_000_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => (50_000_000, HOST_CTRL2_UHS_SDR25),
            ClockSpeed::Sdr50 => (50_000_000, HOST_CTRL2_UHS_SDR50),
            ClockSpeed::Ddr50 => (50_000_000, HOST_CTRL2_UHS_DDR50),
            ClockSpeed::Sdr104 => (104_000_000, HOST_CTRL2_UHS_SDR104),
            ClockSpeed::Hs200 => (200_000_000, HOST_CTRL2_UHS_SDR104),
            _ => return Err(sdmmc_host::Error::Unsupported),
        };
        if self.ext_clock.is_none() && self.base_clock_hz() == 0 {
            return Err(sdmmc_host::Error::Controller);
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
    ) -> Result<BusRequestState, sdmmc_host::Error> {
        if matches!(voltage, SignalVoltage::V180) && !self.support_1v8 {
            return Err(sdmmc_host::Error::Unsupported);
        }
        if matches!(voltage, SignalVoltage::V180) && self.timer.is_none() {
            return Err(sdmmc_host::Error::Unsupported);
        }
        match voltage {
            SignalVoltage::V330 | SignalVoltage::V180 => Ok(BusRequestState::SetSignalVoltage(
                SdhciVoltageState::DisableClock(voltage),
            )),
            SignalVoltage::V120 => Err(sdmmc_host::Error::Unsupported),
            _ => Err(sdmmc_host::Error::Unsupported),
        }
    }

    fn prepare_host2_tuning(
        &self,
        command: sdmmc_host::Command,
        block_size: core::num::NonZeroU16,
    ) -> Result<BusRequestState, sdmmc_host::Error> {
        if command.index != 19 && command.index != 21 {
            return Err(sdmmc_host::Error::InvalidArgument);
        }
        let expected =
            if command.index == 21 && self.read_u8(REG_HOST_CONTROL1) & HOST_CTRL1_8BIT != 0 {
                sdmmc_protocol::cmd::MMC_TUNING_BLOCK_SIZE_8BIT
            } else {
                sdmmc_protocol::cmd::SD_TUNING_BLOCK_SIZE
            };
        if u32::from(block_size.get()) != expected {
            return Err(sdmmc_host::Error::InvalidArgument);
        }
        Ok(BusRequestState::ExecuteTuning(SdhciTuningState::Start {
            cmd_index: command.index,
            block_size: block_size.get(),
        }))
    }

    pub(super) fn advance_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
        cause: sdmmc_host::ProgressCause,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        let register_only = !matches!(state, BusRequestState::ExecuteTuning(_));
        if register_only && cause == sdmmc_host::ProgressCause::AcknowledgedIrq {
            return Ok(sdmmc_host::RequestProgress::RegisterPending {
                retry_after: SDHCI_REGISTER_RETRY_DELAY,
            });
        }
        if !register_only && cause == sdmmc_host::ProgressCause::RegisterRetry {
            return Ok(sdmmc_host::RequestProgress::WaitingForIrq);
        }
        let progress = match state {
            BusRequestState::Reset {
                mask,
                phase,
                was_irq_enabled,
                started,
                polls,
            } => self.advance_host2_reset(*mask, *phase, *was_irq_enabled, started, polls),
            BusRequestState::PowerOn => {
                self.set_power(POWER_330);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::PowerOff => {
                self.write_u8(REG_POWER_CONTROL, 0);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.advance_host2_clock(clock),
            BusRequestState::SetBusWidth(width) => {
                self.apply_bus_width(*width).map_err(map_protocol_error)?;
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => self.advance_host2_voltage(voltage),
            BusRequestState::ExecuteTuning(tuning) => self.advance_host2_tuning(tuning),
        }?;
        if register_only && matches!(progress, sdmmc_host::RequestProgress::WaitingForIrq) {
            Ok(sdmmc_host::RequestProgress::RegisterPending {
                retry_after: SDHCI_REGISTER_RETRY_DELAY,
            })
        } else {
            Ok(progress)
        }
    }

    fn advance_host2_reset(
        &mut self,
        mask: u8,
        phase: Phase,
        was_irq_enabled: bool,
        started: &mut bool,
        polls: &mut u32,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        if !*started {
            if mask == RESET_ALL {
                self.call_before_reset_all_hook()
                    .map_err(map_protocol_error)?;
            }
            self.write_u8(REG_SOFTWARE_RESET, mask);
            *started = true;
        }
        if self.read_u8(REG_SOFTWARE_RESET) & mask == 0 {
            if mask == RESET_ALL {
                self.call_after_reset_hook().map_err(map_protocol_error)?;
                self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
                self.clear_cached_irq_status();
                self.restore_completion_irq_after_reset(was_irq_enabled);
                self.dma_poisoned = false;
            }
            return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
        }
        if *polls >= SDHCI_RESET_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(phase))));
        }
        *polls += 1;
        Ok(sdmmc_host::RequestProgress::WaitingForIrq)
    }

    fn advance_host2_clock(
        &mut self,
        state: &mut SdhciClockState,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
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
                if let Some(enabled) = high_speed {
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
                    *state = SdhciClockState::InternalWaitStable { polls: 0 };
                }
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciClockState::ExternalSetClock { target_hz } => {
                let clock = self
                    .ext_clock
                    .as_ref()
                    .ok_or(sdmmc_host::Error::Controller)?;
                let effective_hz = clock.effective_clock_hz(target_hz);
                clock.set_clock(effective_hz).map_err(map_protocol_error)?;
                *state = SdhciClockState::ExternalPrepareHost {
                    target_hz: effective_hz,
                };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciClockState::ExternalPrepareHost { target_hz } => {
                let clock = self.ext_clock.take().ok_or(sdmmc_host::Error::Controller)?;
                let result = clock.prepare_host_clock(self, target_hz);
                self.ext_clock = Some(clock);
                result.map_err(map_protocol_error)?;
                *state = SdhciClockState::ExternalStart { target_hz };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciClockState::ExternalStart { target_hz } => {
                self.start_passthrough_clock(target_hz);
                *state = SdhciClockState::ExternalEnable { polls: 0 };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciClockState::ExternalEnable { ref mut polls }
            | SdhciClockState::InternalWaitStable { ref mut polls } => {
                self.advance_clock_stable(polls)
            }
        }
    }

    fn start_internal_clock(&mut self, target_hz: u32) -> Result<(), sdmmc_host::Error> {
        self.write_u16(REG_CLOCK_CONTROL, 0);
        if target_hz == 0 {
            return Ok(());
        }
        let base_clock_hz = self.base_clock_hz();
        if base_clock_hz == 0 {
            return Err(sdmmc_host::Error::Controller);
        }
        let div = sdhci_clock_divisor(base_clock_hz, target_hz);
        let clk_ctrl = ((div & 0xFF) << 8) | ((div & 0x300) >> 2) | CLOCK_INTERNAL_ENABLE;
        self.write_u16(REG_CLOCK_CONTROL, clk_ctrl);
        Ok(())
    }

    fn advance_clock_stable(
        &mut self,
        polls: &mut u32,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        if clock & CLOCK_INTERNAL_ENABLE == 0 {
            return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
        }
        if clock & CLOCK_INTERNAL_STABLE != 0 {
            self.write_u16(REG_CLOCK_CONTROL, clock | CLOCK_SD_ENABLE);
            return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
        }
        if *polls >= SDHCI_CLOCK_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        *polls += 1;
        Ok(sdmmc_host::RequestProgress::WaitingForIrq)
    }

    fn advance_host2_voltage(
        &mut self,
        state: &mut SdhciVoltageState,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
        match *state {
            SdhciVoltageState::DisableClock(voltage) => {
                self.disable_sd_clock();
                *state = SdhciVoltageState::SwitchControllerAndRail(voltage);
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciVoltageState::SwitchControllerAndRail(voltage) => {
                if matches!(voltage, SignalVoltage::V180) && !self.dat_3_0_lines_low() {
                    self.rollback_host2_voltage();
                    return Ok(sdmmc_host::RequestProgress::Complete(Err(
                        sdmmc_host::Error::Controller,
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
                    SignalVoltage::V120 => return Err(sdmmc_host::Error::Unsupported),
                    _ => return Err(sdmmc_host::Error::Unsupported),
                }
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
                *state = SdhciVoltageState::WaitVsw {
                    voltage,
                    deadline_ms: self
                        .timer
                        .map(HostTimer::now_ms)
                        .map(|now| now.saturating_add(SDHCI_VOLTAGE_SWITCH_DELAY_MS)),
                };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciVoltageState::WaitVsw {
                voltage,
                deadline_ms,
            } => {
                if deadline_ms.is_none()
                    || deadline_ms
                        .zip(self.timer.map(HostTimer::now_ms))
                        .is_some_and(|(deadline, now)| now >= deadline)
                {
                    *state = SdhciVoltageState::EnableClock(voltage);
                }
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciVoltageState::EnableClock(voltage) => {
                let cur = self.read_u16(REG_CLOCK_CONTROL);
                self.write_u16(REG_CLOCK_CONTROL, cur | CLOCK_SD_ENABLE);
                *state = SdhciVoltageState::VerifyDatLines(voltage);
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciVoltageState::VerifyDatLines(voltage) => {
                if matches!(voltage, SignalVoltage::V180) && !self.dat_3_0_lines_high() {
                    self.rollback_host2_voltage();
                    return Ok(sdmmc_host::RequestProgress::Complete(Err(
                        sdmmc_host::Error::Controller,
                    )));
                }
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
        }
    }

    fn advance_host2_tuning(
        &mut self,
        state: &mut SdhciTuningState,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::Error> {
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
                    polls: 0,
                };
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
            SdhciTuningState::Wait {
                cmd_index,
                ref mut polls,
            } => {
                let status = self.read_u16(REG_HOST_CONTROL2);
                if status & HOST_CTRL2_EXECUTE_TUNING == 0 {
                    if status & HOST_CTRL2_SAMPLING_CLOCK_SELECT != 0 {
                        return Ok(sdmmc_host::RequestProgress::Complete(Ok(())));
                    }
                    return Err(map_protocol_error(Error::BadResponse(
                        ErrorContext::for_cmd(Phase::Init, cmd_index),
                    )));
                }
                if *polls >= SDHCI_TUNING_POLLS {
                    self.write_u16(REG_HOST_CONTROL2, status & !HOST_CTRL2_EXECUTE_TUNING);
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::for_cmd(
                        Phase::Init,
                        cmd_index,
                    ))));
                }
                *polls += 1;
                Ok(sdmmc_host::RequestProgress::WaitingForIrq)
            }
        }
    }

    pub(super) fn abort_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<(), sdmmc_host::Error> {
        match state {
            BusRequestState::Reset { mask, started, .. } if *started => {
                if !self.reset_with_mask_best_effort(*mask) {
                    return Err(sdmmc_host::Error::Timeout);
                }
            }
            BusRequestState::SetClock(_) => self.reset_controller_for_host2_abort()?,
            BusRequestState::SetSignalVoltage(_) => self.rollback_host2_voltage(),
            BusRequestState::ExecuteTuning(SdhciTuningState::Wait { .. }) => {
                let ctrl2 = self.read_u16(REG_HOST_CONTROL2) & !HOST_CTRL2_EXECUTE_TUNING;
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
                self.reset_controller_for_host2_abort()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn reset_controller_for_host2_abort(&mut self) -> Result<(), sdmmc_host::Error> {
        let was_irq_enabled = self.completion_irq_enabled();
        self.call_before_reset_all_hook()
            .map_err(map_protocol_error)?;
        self.write_u8(REG_SOFTWARE_RESET, RESET_ALL);
        if !self.reset_with_mask_best_effort(RESET_ALL) {
            return Err(sdmmc_host::Error::Timeout);
        }
        self.call_after_reset_hook().map_err(map_protocol_error)?;
        self.write_interrupt_status(NORMAL_INT_CLEAR_ALL, ERROR_INT_CLEAR_ALL);
        self.clear_cached_irq_status();
        self.restore_completion_irq_after_reset(was_irq_enabled);
        self.command_state = command::CommandState::Idle;
        Ok(())
    }

    pub(crate) fn restore_completion_irq_after_reset(&mut self, was_irq_enabled: bool) {
        self.enable_interrupt_status_capture();
        if was_irq_enabled {
            self.enable_completion_irq();
        }
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

    fn reset_with_mask_best_effort(&mut self, mask: u8) -> bool {
        for _ in 0..SDHCI_RESET_POLLS {
            if self.read_u8(REG_SOFTWARE_RESET) & mask == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    pub(crate) fn apply_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
        ctrl &= !(HOST_CTRL1_4BIT | HOST_CTRL1_8BIT);
        match width {
            BusWidth::Bit1 => {}
            BusWidth::Bit4 => ctrl |= HOST_CTRL1_4BIT,
            BusWidth::Bit8 => ctrl |= HOST_CTRL1_8BIT,
        }
        self.write_u8(REG_HOST_CONTROL1, ctrl);
        Ok(())
    }
}
