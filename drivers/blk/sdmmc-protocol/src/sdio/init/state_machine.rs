use sdio_host2::ProgressCause;

use super::*;

mod identify;
mod mmc;
mod sd;

impl<H: SdioIrqHost> SdioSdmmc<H> {
    pub(crate) fn init_wait_kind(&self, request: &SdioInitRequest<H>) -> SdioInitWait {
        match (request.wait_kind(), self.host.progress_wait()) {
            (SdioInitWait::Irq, crate::sdio::host::HostProgressWait::Register { .. }) => {
                SdioInitWait::Register
            }
            (protocol, _) => protocol,
        }
    }

    #[cfg(any(feature = "rdif", test))]
    pub(crate) fn init_register_retry_after(
        &self,
        request: &SdioInitRequest<H>,
    ) -> Option<core::time::Duration> {
        match (request.wait_kind(), self.host.progress_wait()) {
            (
                SdioInitWait::Register | SdioInitWait::Irq,
                crate::sdio::host::HostProgressWait::Register { retry_after },
            ) => Some(retry_after),
            _ => None,
        }
    }

    /// Submit SD/MMC card initialization without waiting for completion.
    ///
    /// # Event contract
    ///
    /// The caller may advance the returned [`SdioInitRequest`] only after the
    /// event reported by [`SdioInitRequest::wait_kind`]. An IRQ state requires
    /// an acknowledged device interrupt; a register state may advance in task
    /// context under the caller's unified deadline. `take_needs_pace` requests
    /// the bounded delay between ACMD41/CMD1 power-up retries. It is never a
    /// fallback completion retry.
    pub fn submit_init(&mut self) -> Result<SdioInitRequest<H>, Error> {
        self.submit_init_with_preference(CardInitPreference::SdFirst)
    }

    /// Submit SD/MMC card initialization with a caller-selected probe order.
    pub fn submit_init_with_preference(
        &mut self,
        preference: CardInitPreference,
    ) -> Result<SdioInitRequest<H>, Error> {
        debug!("sdio: init starting");
        let scratch = SdioInitScratch::new(self.host.inner().device_dma()?)?;
        Ok(SdioInitRequest::new(preference, scratch))
    }

    fn submit_init_bus_op(
        &mut self,
        request: &mut SdioInitRequest<H>,
        op: SdioBusOp,
        next: SdioInitState,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        info!("sdio: submit bus op {:?}", op);
        request.bus_request = Some(self.host.submit_bus_op(op)?);
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(OperationProgress::Pending)
    }

    fn advance_init_bus_op(
        &mut self,
        request: &mut SdioInitRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
        match self.host.advance_bus_op(&mut bus_request, cause) {
            Ok(OperationProgress::Pending) => {
                request.bus_request = Some(bus_request);
                Ok(OperationProgress::Pending)
            }
            Ok(OperationProgress::Complete(())) => {
                request.active_bus_op = None;
                Ok(OperationProgress::Complete(()))
            }
            Err(err) => {
                warn!(
                    "sdio: init bus op {:?} failed in state {:?}: {:?}",
                    request.active_bus_op, request.state, err
                );
                request.active_bus_op = None;
                Err(err)
            }
        }
    }

    fn submit_init_bus_op_direct(
        &mut self,
        request: &mut SdioInitRequest<H>,
        op: SdioBusOp,
        next: SdioInitState,
    ) -> Result<(), Error> {
        info!("sdio: submit bus op {:?}", op);
        request.bus_request = Some(self.host.submit_bus_op(op)?);
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(())
    }

    fn advance_init_bus_op_then(
        &mut self,
        request: &mut SdioInitRequest<H>,
        cause: ProgressCause,
        complete: impl FnOnce(
            &mut Self,
            &mut SdioInitRequest<H>,
        ) -> Result<OperationProgress<CardInfo>, Error>,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        match self.advance_init_bus_op(request, cause)? {
            OperationProgress::Pending => Ok(OperationProgress::Pending),
            OperationProgress::Complete(()) => complete(self, request),
        }
    }

    /// Advance a submitted initialization request without blocking.
    ///
    /// On any terminal `Err` the controller is reset back toward an
    /// identification-mode-compatible state (1-bit bus, 400 kHz clock, 3.3 V
    /// signaling) so a retry from a fresh [`submit_init`](Self::submit_init)
    /// starts from a known baseline. `Ok(OperationProgress::Pending)` does not
    /// trigger the reset; only terminal failures do.
    pub fn advance_init_request(
        &mut self,
        request: &mut SdioInitRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        let protocol_wait = request.wait_kind();
        let effective_wait = self.init_wait_kind(request);
        let matches_wait = matches!(
            (protocol_wait, cause),
            (SdioInitWait::Irq, ProgressCause::AcknowledgedIrq)
        ) || matches!(
            (effective_wait, cause),
            (
                SdioInitWait::Register,
                ProgressCause::Submitted | ProgressCause::RegisterRetry
            )
        );
        if !matches_wait {
            return Ok(OperationProgress::Pending);
        }
        match self.advance_init_inner(request, cause) {
            Ok(progress) => Ok(progress),
            Err(err) => {
                warn!(
                    "sdio: init state {:?} aborted ({:?}), resetting host",
                    request.state, err
                );
                if let Err(recovery) = self.abort_init_request(request) {
                    warn!("sdio: init request recovery failed: {recovery:?}");
                }
                Err(err)
            }
        }
    }

    fn advance_init_inner(
        &mut self,
        request: &mut SdioInitRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        match request.state {
            SdioInitState::ResetHost
            | SdioInitState::PollResetHost
            | SdioInitState::PowerOn
            | SdioInitState::PollPowerOn
            | SdioInitState::ResetVoltage
            | SdioInitState::PollResetVoltage
            | SdioInitState::ResetBusWidth
            | SdioInitState::ResetClock
            | SdioInitState::PostIdentificationClockDelay
            | SdioInitState::SubmitCmd0
            | SdioInitState::PollCmd0
            | SdioInitState::PollCmd8
            | SdioInitState::PollAcmd41Cmd55
            | SdioInitState::PollAcmd41
            | SdioInitState::SubmitAcmd41Retry
            | SdioInitState::PollMmcInitial
            | SdioInitState::PollMmcReady
            | SdioInitState::SubmitMmcReadyRetry
            | SdioInitState::PollCmd2
            | SdioInitState::PollCmd3
            | SdioInitState::PollCmd9
            | SdioInitState::PollCmd7
            | SdioInitState::PollSdBusWidthCmd55
            | SdioInitState::PollSdBusWidthAcmd6
            | SdioInitState::PollSdHostBusWidth
            | SdioInitState::FinishCardSetup
            | SdioInitState::PollSdDefaultClock => self.advance_identification(request, cause),
            SdioInitState::PollMmcExtCsd
            | SdioInitState::PollMmcBusWidth
            | SdioInitState::PollMmcHostBusWidth
            | SdioInitState::PrepareMmcSpeed
            | SdioInitState::PollMmcHs200VoltageSwitch
            | SdioInitState::PollMmcHs200Switch
            | SdioInitState::PollMmcHs200Clock
            | SdioInitState::PollMmcHs200Tuning
            | SdioInitState::PollMmcHs200Status
            | SdioInitState::PollMmcHs200RollbackVoltage
            | SdioInitState::PollMmcHs200RollbackClock
            | SdioInitState::PollMmcHs52Switch
            | SdioInitState::PollMmcHighSpeedClock
            | SdioInitState::PollMmcCacheEnable => self.advance_mmc_setup(request, cause),
            SdioInitState::PrepareSdSpeed
            | SdioInitState::PollSdSwitchFunctionCheck
            | SdioInitState::PollSdVoltageSwitch
            | SdioInitState::PollSdSignalVoltage
            | SdioInitState::PollSdSetAccessMode
            | SdioInitState::PollSdClock
            | SdioInitState::PollSdTuning
            | SdioInitState::PollSdStatus => self.advance_sd_speed_setup(request, cause),
            SdioInitState::Complete => self.finish_init(request),
        }
    }

    fn finish_init(
        &mut self,
        request: &mut SdioInitRequest<H>,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        let kind = request.kind.ok_or(Error::InvalidArgument)?;
        let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
        let ext_csd_timing = request.parsed_ext_csd.as_ref().map(|csd| csd.timing());
        let ext_csd_bus_width = request.parsed_ext_csd.as_ref().map(|csd| csd.bus_width());
        info!(
            "sdio: init done kind={:?} sd_v2={} high_capacity={} rca={:#x} ocr={:#x} \
             host_bus_width={:?} ext_csd_bus_width={:?} ext_csd_timing={:?}",
            kind,
            request.sd_v2,
            self.high_capacity,
            self.rca,
            ocr.raw,
            self.bus_width,
            ext_csd_bus_width,
            ext_csd_timing
        );
        Ok(OperationProgress::Complete(CardInfo {
            kind,
            sd_v2: request.sd_v2,
            high_capacity: self.high_capacity,
            ocr: ocr.raw,
            rca: self.rca,
            capacity_blocks: request.capacity_blocks,
            cid: request.cid,
            ext_csd: request.parsed_ext_csd.take(),
        }))
    }

    /// Best-effort host + driver reset after a failed or abandoned init.
    ///
    /// Init can leave the controller in any number of partially-programmed
    /// states: 4-bit/8-bit bus already negotiated, clock raised to HS@52,
    /// HOST_CONTROL2 UHS bits set from a HS200 attempt, 1.8 V signaling
    /// armed. None of those are safe defaults for a subsequent retry that
    /// expects to start by replaying CMD0 in identification mode.
    ///
    /// This helper:
    ///
    /// - Asks the host to drop back to identification clock, 1-bit bus, and
    ///   3.3 V signaling. Errors from each call are swallowed — we're
    ///   already on the error path and want maximum cleanup, not a second
    ///   failure mid-recovery.
    /// - Clears the driver's cached card state (RCA, kind, bus width,
    ///   high-capacity flag) so subsequent calls don't act on stale data
    ///   from the aborted card.
    ///
    /// Idempotent: calling it twice or on a fresh driver is a no-op
    /// modulo the (already-defaulted) field stores.
    pub(crate) fn abort_init_request(
        &mut self,
        request: &mut SdioInitRequest<H>,
    ) -> Result<(), Error> {
        let mut first_error = None;
        let mut remember = |result: Result<(), Error>| {
            if let Err(error) = result
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        };

        if let Some(mut ext_csd) = request.ext_csd_request.take() {
            let (result, completed) = self.abort_data_request(&mut ext_csd.inner);
            match completed {
                Some(completed) => request.ext_csd_buf = Some(completed.into_cpu_buffer()),
                None if result.is_ok() => {
                    remember(Err(Error::BusError(ErrorContext::new(Phase::Init))));
                }
                None => {}
            }
            remember(result);
        }
        if let Some(mut switch_function) = request.switch_function_request.take() {
            let (result, completed) = self.abort_data_request(&mut switch_function.inner);
            match completed {
                Some(completed) => {
                    request.switch_status_buf = Some(completed.into_cpu_buffer());
                }
                None if result.is_ok() => {
                    remember(Err(Error::BusError(ErrorContext::new(Phase::Init))));
                }
                None => {}
            }
            remember(result);
        }
        if let Some(mut bus_request) = request.bus_request.take() {
            remember(self.host.abort_bus_request(&mut bus_request));
        }
        request.active_bus_op = None;

        request.mmc_switch_request = None;
        request.status_request = None;
        request.command_request = None;
        remember(self.host.abort_command_request());

        self.abort_init();
        first_error.map_or(Ok(()), Err)
    }

    fn abort_init(&mut self) {
        let _ = self.host.switch_voltage(SignalVoltage::V330);
        let _ = self.host.set_clock(ClockSpeed::Identification);
        let _ = self.host.set_bus_width(BusWidth::Bit1);
        self.rca = 0;
        self.high_capacity = false;
        self.bus_width = BusWidth::Bit1;
        self.kind = CardKind::Sd;
    }
}

fn submit_mmc_bus_width_or_continue<H: SdioIrqHost>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<H>,
    width: BusWidth,
) -> Result<OperationProgress<CardInfo>, Error> {
    let value: u8 = match width {
        BusWidth::Bit1 => 0,
        BusWidth::Bit4 => 1,
        BusWidth::Bit8 => 2,
    };
    request.current_bus_width = width;
    request.mmc_switch_request =
        Some(driver.submit_mmc_switch(0b11, crate::cmd::ext_csd::BUS_WIDTH as u8, value)?);
    request.state = SdioInitState::PollMmcBusWidth;
    Ok(OperationProgress::Pending)
}

fn handle_mmc_host_bus_width_error<H: SdioIrqHost>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<H>,
    err: Error,
) -> Result<OperationProgress<CardInfo>, Error> {
    request.bus_request = None;
    if matches!(request.current_bus_width, BusWidth::Bit8) {
        debug!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
        submit_mmc_bus_width_or_continue(driver, request, BusWidth::Bit4)
    } else if matches!(request.current_bus_width, BusWidth::Bit4) {
        debug!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
        request.state = SdioInitState::PrepareMmcSpeed;
        Ok(OperationProgress::Pending)
    } else {
        Err(err)
    }
}

fn submit_next_sd_access_mode<H: SdioIrqHost>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<H>,
    status: SwitchStatus,
) -> Result<OperationProgress<CardInfo>, Error> {
    let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
    let candidates = if driver.sd_uhs_selection_enabled && ocr.s18a() {
        &[
            SdAccessMode::Sdr104,
            SdAccessMode::Sdr50,
            SdAccessMode::Ddr50,
            SdAccessMode::HighSpeed,
        ][..]
    } else {
        &[SdAccessMode::HighSpeed][..]
    };

    while request.sd_access_index < candidates.len() {
        let mode = candidates[request.sd_access_index];
        request.sd_access_index += 1;
        if !status.access_mode_supported(mode.function()) {
            continue;
        }
        if matches!(mode, SdAccessMode::HighSpeed) {
            debug!("sdio: trying SD HighSpeed");
        } else {
            debug!("sdio: trying SD {}", mode.name());
        }
        return submit_sd_access_mode(driver, request, mode);
    }

    debug!("sdio: SD card stayed at default speed");
    request.state = SdioInitState::Complete;
    Ok(OperationProgress::Pending)
}

fn submit_sd_access_mode<H: SdioIrqHost>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<H>,
    mode: SdAccessMode,
) -> Result<OperationProgress<CardInfo>, Error> {
    request.current_access_mode = Some(mode);
    if !matches!(mode, SdAccessMode::HighSpeed) && request.ocr.ok_or(Error::InvalidArgument)?.s18a()
    {
        let cmd = crate::cmd::CMD11;
        request.command_request = Some(driver.submit_command_request(&cmd)?);
        request.state = SdioInitState::PollSdVoltageSwitch;
        return Ok(OperationProgress::Pending);
    }

    submit_sd_access_mode_switch(driver, request, mode)
}

fn submit_sd_access_mode_switch<H: SdioIrqHost>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<H>,
    mode: SdAccessMode,
) -> Result<OperationProgress<CardInfo>, Error> {
    submit_switch_function_owned(
        driver,
        request,
        &crate::cmd::cmd6_sd_access_mode(true, mode.function()),
        SdioInitState::PollSdSetAccessMode,
    )
}

fn submit_switch_function_owned<H: SdioIrqHost>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<H>,
    command: &Command,
    next: SdioInitState,
) -> Result<OperationProgress<CardInfo>, Error> {
    let buffer = request
        .switch_status_buf
        .take()
        .ok_or(Error::InvalidArgument)?;
    match driver.submit_switch_function_dma(command, buffer) {
        Ok(switch_request) => {
            request.switch_function_request = Some(switch_request);
            request.state = next;
            Ok(OperationProgress::Pending)
        }
        Err(error) => {
            let protocol_error = error.error;
            request.switch_status_buf = Some(error.into_buffer().into_cpu_buffer());
            Err(protocol_error)
        }
    }
}

fn finish_switch_function<H: SdioIrqHost>(
    request: &mut SdioInitRequest<H>,
) -> Result<SwitchStatus, Error> {
    let mut switch_request = request
        .switch_function_request
        .take()
        .ok_or(Error::InvalidArgument)?;
    let completed = switch_request
        .inner
        .take_completed_dma()
        .ok_or(Error::BusError(ErrorContext::new(Phase::Init)))?;
    let buffer = completed.into_cpu_buffer();
    let bytes: [u8; 64] = buffer
        .as_slice_cpu()
        .try_into()
        .map_err(|_| Error::InvalidArgument)?;
    let status = SwitchStatus::from_raw(bytes);
    request.switch_status_buf = Some(buffer);
    Ok(status)
}

fn current_switch_status<H: SdioIrqHost>(
    request: &SdioInitRequest<H>,
) -> Result<SwitchStatus, Error> {
    let buffer = request
        .switch_status_buf
        .as_ref()
        .ok_or(Error::InvalidArgument)?;
    let bytes: [u8; 64] = buffer
        .as_slice_cpu()
        .try_into()
        .map_err(|_| Error::InvalidArgument)?;
    Ok(SwitchStatus::from_raw(bytes))
}
