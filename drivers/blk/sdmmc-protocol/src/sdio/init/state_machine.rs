use super::*;

mod identify;
mod mmc;
mod sd;

impl<H: SdioHost> SdioSdmmc<H> {
    /// Submit SD/MMC card initialization without waiting for completion.
    ///
    /// # Event contract
    ///
    /// The caller may advance the returned [`SdioInitRequest`] only after the
    /// event reported by [`SdioInitRequest::wait_kind`]. An IRQ state requires
    /// an acknowledged device interrupt; a register state may advance in task
    /// context under the caller's unified deadline. `take_needs_pace` requests
    /// the bounded delay between ACMD41/CMD1 power-up retries. It is never a
    /// fallback completion-polling interval.
    pub fn submit_init<'a>(
        &mut self,
        scratch: &'a mut SdioInitScratch,
    ) -> Result<SdioInitRequest<'a, H>, Error>
    where
        H: 'a,
    {
        self.submit_init_with_preference(CardInitPreference::SdFirst, scratch)
    }

    /// Submit SD/MMC card initialization with a caller-selected probe order.
    pub fn submit_init_with_preference<'a>(
        &mut self,
        preference: CardInitPreference,
        scratch: &'a mut SdioInitScratch,
    ) -> Result<SdioInitRequest<'a, H>, Error>
    where
        H: 'a,
    {
        debug!("sdio: init starting");
        Ok(SdioInitRequest::new(preference, scratch))
    }

    fn submit_init_bus_op<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        op: SdioBusOp,
        next: SdioInitState,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        info!("sdio: submit bus op {:?}", op);
        request.bus_request = Some(self.host.submit_bus_op(op)?);
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(OperationPoll::Pending)
    }

    fn poll_init_bus_op<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<()>, Error> {
        let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
        match self.host.poll_bus_op(&mut bus_request) {
            Ok(OperationPoll::Pending) => {
                request.bus_request = Some(bus_request);
                Ok(OperationPoll::Pending)
            }
            Ok(OperationPoll::Complete(())) => {
                request.active_bus_op = None;
                Ok(OperationPoll::Complete(()))
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

    fn submit_init_bus_op_direct<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        op: SdioBusOp,
        next: SdioInitState,
    ) -> Result<(), Error> {
        info!("sdio: submit bus op {:?}", op);
        request.bus_request = Some(self.host.submit_bus_op(op)?);
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(())
    }

    fn poll_init_bus_op_then<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        complete: impl FnOnce(
            &mut Self,
            &mut SdioInitRequest<'a, H>,
        ) -> Result<OperationPoll<CardInfo>, Error>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match self.poll_init_bus_op(request)? {
            OperationPoll::Pending => Ok(OperationPoll::Pending),
            OperationPoll::Complete(()) => complete(self, request),
        }
    }

    /// Advance a submitted initialization request without blocking.
    ///
    /// On any terminal `Err` the controller is reset back toward an
    /// identification-mode-compatible state (1-bit bus, 400 kHz clock, 3.3 V
    /// signaling) so a retry from a fresh [`submit_init`](Self::submit_init)
    /// starts from a known baseline. `Ok(OperationPoll::Pending)` does not
    /// trigger the reset; only terminal failures do.
    pub fn poll_init_request<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match self.poll_init_inner(request) {
            Ok(progress) => Ok(progress),
            Err(err) => {
                warn!("sdio: init aborted ({:?}), resetting host", err);
                self.abort_init();
                Err(err)
            }
        }
    }

    fn poll_init_inner<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
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
            | SdioInitState::PollMmcInitial
            | SdioInitState::PollMmcReady
            | SdioInitState::PollCmd2
            | SdioInitState::PollCmd3
            | SdioInitState::PollCmd9
            | SdioInitState::PollCmd7
            | SdioInitState::PollSdBusWidthCmd55
            | SdioInitState::PollSdBusWidthAcmd6
            | SdioInitState::PollSdHostBusWidth
            | SdioInitState::FinishCardSetup
            | SdioInitState::PollSdDefaultClock => self.poll_identification(request),
            SdioInitState::PollMmcExtCsd
            | SdioInitState::PollMmcBusWidth
            | SdioInitState::PollMmcHostBusWidth
            | SdioInitState::PrepareMmcSpeed
            | SdioInitState::PollMmcHs200VoltageSwitch
            | SdioInitState::PollMmcHs200Switch
            | SdioInitState::PollMmcHs200Clock
            | SdioInitState::PollMmcHs200Tuning
            | SdioInitState::PollMmcHs200Status
            | SdioInitState::PollMmcHs52Switch
            | SdioInitState::PollMmcHighSpeedClock
            | SdioInitState::PollMmcCacheEnable => self.poll_mmc_setup(request),
            SdioInitState::PrepareSdSpeed
            | SdioInitState::PollSdSwitchFunctionCheck
            | SdioInitState::PollSdVoltageSwitch
            | SdioInitState::PollSdSignalVoltage
            | SdioInitState::PollSdSetAccessMode
            | SdioInitState::PollSdClock
            | SdioInitState::PollSdTuning
            | SdioInitState::PollSdStatus => self.poll_sd_speed_setup(request),
            SdioInitState::Complete => self.finish_init(request),
        }
    }

    fn finish_init<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
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
        Ok(OperationPoll::Complete(CardInfo {
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
    fn abort_init(&mut self) {
        let _ = self.host.switch_voltage(SignalVoltage::V330);
        let _ = self.host.set_clock(ClockSpeed::Identification);
        let _ = self.host.set_bus_width(BusWidth::Bit1);
        self.rca = 0;
        self.high_capacity = false;
        self.bus_width = BusWidth::Bit1;
        self.kind = CardKind::Sd;
    }

    /// Best-effort rollback after a failed HS200 attempt. Drops the
    /// controller clock back to default speed; the outer `init` will
    /// then re-program HS_TIMING=1 + HighSpeed in its fallback branch.
    /// Errors are deliberately swallowed — we're already on the error
    /// path and want to give the rest of `init` the best shot at
    /// recovering.
    fn rollback_to_hs_compat(&mut self) {
        // Drop any 1.8 V signaling the HS200 attempt may have armed on the
        // controller. Without this, the IO sampling stays at the 1.8 V
        // reference while we drive the bus back at 3.3 V, so the very next
        // data transfer (e.g. the FS layer's CMD17 at LBA 0) times out.
        let _ = self.host.switch_voltage(SignalVoltage::V330);
        let _ = self.host.set_clock(ClockSpeed::Default);
    }
}

fn submit_mmc_bus_width_or_continue<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    width: BusWidth,
) -> Result<OperationPoll<CardInfo>, Error> {
    let value: u8 = match width {
        BusWidth::Bit1 => 0,
        BusWidth::Bit4 => 1,
        BusWidth::Bit8 => 2,
        _ => return Err(Error::UnsupportedCommand),
    };
    request.current_bus_width = width;
    request.mmc_switch_request =
        Some(driver.submit_mmc_switch(0b11, crate::cmd::ext_csd::BUS_WIDTH as u8, value)?);
    request.state = SdioInitState::PollMmcBusWidth;
    Ok(OperationPoll::Pending)
}

fn handle_mmc_host_bus_width_error<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    err: Error,
) -> Result<OperationPoll<CardInfo>, Error> {
    request.bus_request = None;
    if matches!(request.current_bus_width, BusWidth::Bit8) {
        debug!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
        submit_mmc_bus_width_or_continue(driver, request, BusWidth::Bit4)
    } else if matches!(request.current_bus_width, BusWidth::Bit4) {
        debug!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
        request.state = SdioInitState::PrepareMmcSpeed;
        Ok(OperationPoll::Pending)
    } else {
        Err(err)
    }
}

fn submit_next_sd_access_mode<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    status: SwitchStatus,
) -> Result<OperationPoll<CardInfo>, Error> {
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
    Ok(OperationPoll::Pending)
}

fn submit_sd_access_mode<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    mode: SdAccessMode,
) -> Result<OperationPoll<CardInfo>, Error> {
    request.current_access_mode = Some(mode);
    if !matches!(mode, SdAccessMode::HighSpeed) && request.ocr.ok_or(Error::InvalidArgument)?.s18a()
    {
        let cmd = crate::cmd::CMD11;
        request.command_request = Some(driver.submit_command_request(&cmd)?);
        request.state = SdioInitState::PollSdVoltageSwitch;
        return Ok(OperationPoll::Pending);
    }

    submit_sd_access_mode_switch(driver, request, mode)
}

fn submit_sd_access_mode_switch<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    mode: SdAccessMode,
) -> Result<OperationPoll<CardInfo>, Error> {
    // SAFETY: the prior switch_function_request was either consumed and
    // released in PollSdSwitchFunctionCheck Complete, or never lent (CMD11
    // voltage-switch failure path); release defensively so a re-entered
    // path doesn't keep the slot flagged.
    request.switch_status_buf.release();
    let buf = unsafe { request.switch_status_buf.lend() };
    request.switch_function_request = Some(
        driver
            .submit_switch_function(&crate::cmd::cmd6_sd_access_mode(true, mode.function()), buf)?,
    );
    request.state = SdioInitState::PollSdSetAccessMode;
    Ok(OperationPoll::Pending)
}
