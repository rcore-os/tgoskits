use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_sd_speed_state<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match request.state {
            SdioInitState::PrepareSdSpeed => {
                // SAFETY: see ext_csd lend above; release happens on the
                // PollSdSwitchFunctionCheck Complete arm below.
                let buf = unsafe { request.switch_status_buf.lend() };
                let switch_request =
                    self.submit_switch_function(&crate::cmd::cmd6_sd_access_mode(false, 0), buf)?;
                request.switch_function_request = Some(switch_request);
                request.state = SdioInitState::PollSdSwitchFunctionCheck;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollSdSwitchFunctionCheck => {
                match self.poll_init_switch_function(request, now_ns) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        // SAFETY: just released above; host promised the data
                        // phase is done via DataCommandPoll::Complete.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        debug!(
                            "sdio: SD access mode support hs={} sdr50={} sdr104={} ddr50={} \
                             s18a={}",
                            status.access_mode_supported(SdAccessMode::HighSpeed.function()),
                            status.access_mode_supported(SdAccessMode::Sdr50.function()),
                            status.access_mode_supported(SdAccessMode::Sdr104.function()),
                            status.access_mode_supported(SdAccessMode::Ddr50.function()),
                            request.ocr.ok_or(Error::InvalidArgument)?.s18a()
                        );
                        request.sd_access_index = 0;
                        submit_next_sd_access_mode(self, request, status)
                    }
                    Err(err) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        warn!("sdio: SD speed selection skipped ({:?})", err);
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollSdVoltageSwitch => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_command_request(request, now_ns) {
                    Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(CommandResponsePoll::Complete(_)) => {
                        request.command_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SwitchVoltage(SignalVoltage::V180))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollSdSignalVoltage;
                                Ok(OperationPoll::Pending)
                            }
                            Err(err) => {
                                warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                // SAFETY: no switch_function_request is in
                                // flight on this branch (CMD11 path uses the
                                // command channel), so the slot is not lent.
                                let status = SwitchStatus::from_raw(unsafe {
                                    *request.switch_status_buf.peek()
                                });
                                submit_next_sd_access_mode(self, request, status)
                            }
                        }
                    }
                    Err(err) => {
                        request.command_request = None;
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: same as above — no in-flight data request.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdSignalVoltage => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_bus_op(request, now_ns) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        submit_sd_access_mode_switch(self, request, mode)
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: no switch_function_request is in flight on
                        // this branch; the switch-status scratch slot was
                        // released after the earlier function-check request.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdSetAccessMode => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_switch_function(request, now_ns) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        // SAFETY: just released above.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        if status.selected_function(1) != mode.function() {
                            warn!("sdio: SD {} failed (function mismatch)", mode.name());
                            submit_next_sd_access_mode(self, request, status)
                        } else {
                            match self.host.submit_bus_op(SdioBusOp::SetClock(mode.clock())) {
                                Ok(bus_request) => {
                                    request.bus_request = Some(bus_request);
                                    request.state = SdioInitState::PollSdClock;
                                    Ok(OperationPoll::Pending)
                                }
                                Err(err) => {
                                    warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                    submit_next_sd_access_mode(self, request, status)
                                }
                            }
                        }
                    }
                    Err(err) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: just released above.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdClock => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_bus_op(request, now_ns) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        if matches!(mode, SdAccessMode::Sdr50 | SdAccessMode::Sdr104) {
                            let block_size = self.sd_tuning_block_size()?;
                            match self.host.submit_bus_op(SdioBusOp::ExecuteTuning {
                                cmd_index: 19,
                                block_size,
                            }) {
                                Ok(bus_request) => {
                                    request.bus_request = Some(bus_request);
                                    request.state = SdioInitState::PollSdTuning;
                                    Ok(OperationPoll::Pending)
                                }
                                Err(err) => {
                                    warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                    // SAFETY: PollSdClock is reached after the
                                    // switch request released the status slot.
                                    let status = SwitchStatus::from_raw(unsafe {
                                        *request.switch_status_buf.peek()
                                    });
                                    submit_next_sd_access_mode(self, request, status)
                                }
                            }
                        } else {
                            let status_request = self.submit_status()?;
                            request.status_request = Some(status_request);
                            request.state = SdioInitState::PollSdStatus;
                            Ok(OperationPoll::Pending)
                        }
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: PollSdClock is reached after the switch
                        // request released the status slot.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdTuning => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_bus_op(request, now_ns) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        let status_request = self.submit_status()?;
                        request.status_request = Some(status_request);
                        request.state = SdioInitState::PollSdStatus;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: PollSdTuning is reached after the switch
                        // request released the status slot.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdStatus => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_status(request, now_ns)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        self.clock = mode.clock();
                        info!("sdio: SD speed selected {:?}", mode.clock());
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                    OperationPoll::Complete(_) => {
                        request.status_request = None;
                        warn!("sdio: SD {} failed (bad status)", mode.name());
                        // SAFETY: PollSdStatus is reached after the switch
                        // request released the slot in PollSdSetAccessMode;
                        // no data request is in flight.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            _ => Err(Error::InvalidArgument),
        }
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

pub(in crate::sdio) fn sd_acmd6_arg(width: BusWidth) -> Result<u32, Error> {
    match width {
        BusWidth::Bit1 => Ok(0),
        BusWidth::Bit4 => Ok(2),
        BusWidth::Bit8 => Err(Error::UnsupportedCommand),
        _ => Err(Error::UnsupportedCommand),
    }
}
