use super::*;

impl<H: SdioIrqHost> SdioSdmmc<H> {
    pub(super) fn advance_mmc_setup(
        &mut self,
        request: &mut SdioInitRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        match request.state {
            SdioInitState::PollMmcExtCsd => {
                let progress = {
                    let ext_request = request
                        .ext_csd_request
                        .as_mut()
                        .ok_or(Error::InvalidArgument)?;
                    self.advance_ext_csd_request(ext_request, cause)
                };
                match progress {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        let mut ext_request = request
                            .ext_csd_request
                            .take()
                            .ok_or(Error::InvalidArgument)?;
                        let completed = ext_request
                            .inner
                            .take_completed_dma()
                            .ok_or(Error::BusError(ErrorContext::new(Phase::Init)))?;
                        let ext_csd = completed.into_cpu_buffer();
                        let bytes: [u8; 512] = ext_csd
                            .as_slice_cpu()
                            .try_into()
                            .map_err(|_| Error::InvalidArgument)?;
                        let csd = crate::ext_csd::ExtCsd::from_bytes(bytes);
                        request.ext_csd_buf = Some(ext_csd);
                        if let Some(sectors) = csd.sector_count() {
                            request.capacity_blocks = Some(sectors as u64);
                            info!("sdio: EXT_CSD sector_count={}", sectors);
                        }
                        request.parsed_ext_csd = Some(csd);
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit8)
                    }
                    Err(error) => {
                        let mut ext_request = request
                            .ext_csd_request
                            .take()
                            .ok_or(Error::InvalidArgument)?;
                        let completed = ext_request
                            .inner
                            .take_completed_dma()
                            .ok_or(Error::BusError(ErrorContext::new(Phase::Init)))?;
                        request.ext_csd_buf = Some(completed.into_cpu_buffer());
                        Err(error)
                    }
                }
            }
            SdioInitState::PollMmcBusWidth => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_mmc_switch_request(switch_request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetBusWidth(request.current_bus_width))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollMmcHostBusWidth;
                                Ok(OperationProgress::Pending)
                            }
                            Err(err) => handle_mmc_host_bus_width_error(self, request, err),
                        }
                    }
                    Err(err) if matches!(request.current_bus_width, BusWidth::Bit8) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit4)
                    }
                    Err(err) if matches!(request.current_bus_width, BusWidth::Bit4) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationProgress::Pending)
                    }
                    Err(err) => Err(err),
                }
            }
            SdioInitState::PollMmcHostBusWidth => {
                let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
                match self.host.advance_bus_op(&mut bus_request, cause) {
                    Ok(OperationProgress::Pending) => {
                        request.bus_request = Some(bus_request);
                        Ok(OperationProgress::Pending)
                    }
                    Ok(OperationProgress::Complete(())) => {
                        self.bus_width = request.current_bus_width;
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationProgress::Pending)
                    }
                    Err(err) => handle_mmc_host_bus_width_error(self, request, err),
                }
            }
            SdioInitState::PrepareMmcSpeed => {
                let dt = request
                    .parsed_ext_csd
                    .as_ref()
                    .ok_or(Error::InvalidArgument)?
                    .device_type();
                if !request.mmc_hs200_attempted
                    && !matches!(self.bus_width, BusWidth::Bit1)
                    && dt.supports_hs200()
                {
                    request.mmc_hs200_attempted = true;
                    match self
                        .host
                        .submit_bus_op(SdioBusOp::SwitchVoltage(SignalVoltage::V180))
                    {
                        Ok(bus_request) => {
                            request.bus_request = Some(bus_request);
                            request.active_bus_op =
                                Some(SdioBusOp::SwitchVoltage(SignalVoltage::V180));
                            request.state = SdioInitState::PollMmcHs200VoltageSwitch;
                            return Ok(OperationProgress::Pending);
                        }
                        // The host has no way to actually drive the IO rail
                        // at 1.8 V (controllers like the rk3568 SDHCI MVP
                        // refuse here on purpose); HS200 requires 1.8 V, so
                        // skip the attempt entirely instead of leaving the
                        // controller's 1.8 V Signaling Enable bit set while
                        // running the bus at 3.3 V.
                        Err(Error::UnsupportedCommand) => {
                            debug!("sdio: host does not support MMC HS200 signal voltage");
                        }
                        Err(err) => debug!("sdio: switch_voltage(V180) failed ({:?})", err),
                    }
                }
                self.prepare_mmc_hs52_or_complete(request)
            }
            SdioInitState::PollMmcHs200VoltageSwitch => {
                match self.advance_init_bus_op(request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        let switch_request = self.submit_mmc_switch(
                            0b11,
                            crate::cmd::ext_csd::HS_TIMING as u8,
                            0x02,
                        )?;
                        request.mmc_switch_request = Some(switch_request);
                        request.state = SdioInitState::PollMmcHs200Switch;
                        Ok(OperationProgress::Pending)
                    }
                    Err(err) => {
                        debug!("sdio: switch_voltage(V180) failed ({:?})", err);
                        self.begin_mmc_hs200_fallback(request)
                    }
                }
            }
            SdioInitState::PollMmcHs200Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_mmc_switch_request(switch_request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetClock(ClockSpeed::Hs200))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.active_bus_op =
                                    Some(SdioBusOp::SetClock(ClockSpeed::Hs200));
                                request.state = SdioInitState::PollMmcHs200Clock;
                                Ok(OperationProgress::Pending)
                            }
                            Err(err) => {
                                debug!("sdio: host refused MMC HS200 clock ({err:?})");
                                self.begin_mmc_hs200_fallback(request)
                            }
                        }
                    }
                    Err(err) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: MMC HS200 switch refused ({:?})", err);
                        self.begin_mmc_hs200_fallback(request)
                    }
                }
            }
            SdioInitState::PollMmcHs200Clock => match self.advance_init_bus_op(request, cause) {
                Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                Ok(OperationProgress::Complete(())) => {
                    let block_size = self.mmc_tuning_block_size()?;
                    match self.host.submit_bus_op(SdioBusOp::ExecuteTuning {
                        cmd_index: 21,
                        block_size,
                    }) {
                        Ok(bus_request) => {
                            request.bus_request = Some(bus_request);
                            request.active_bus_op = Some(SdioBusOp::ExecuteTuning {
                                cmd_index: 21,
                                block_size,
                            });
                            request.state = SdioInitState::PollMmcHs200Tuning;
                            Ok(OperationProgress::Pending)
                        }
                        Err(err) => {
                            debug!("sdio: host refused MMC HS200 tuning ({err:?})");
                            self.begin_mmc_hs200_fallback(request)
                        }
                    }
                }
                Err(err) => {
                    debug!("sdio: MMC HS200 clock failed ({err:?})");
                    self.begin_mmc_hs200_fallback(request)
                }
            },
            SdioInitState::PollMmcHs200Tuning => match self.advance_init_bus_op(request, cause) {
                Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                Ok(OperationProgress::Complete(())) => {
                    let status_request = self.submit_status()?;
                    request.status_request = Some(status_request);
                    request.state = SdioInitState::PollMmcHs200Status;
                    Ok(OperationProgress::Pending)
                }
                Err(err) => {
                    debug!("sdio: MMC HS200 tuning failed ({err:?})");
                    self.begin_mmc_hs200_fallback(request)
                }
            },
            SdioInitState::PollMmcHs200Status => {
                let status_request = request
                    .status_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_status_request(status_request, cause)? {
                    OperationProgress::Pending => Ok(OperationProgress::Pending),
                    OperationProgress::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        info!("sdio: HS200 entry succeeded");
                        self.prepare_mmc_cache_or_complete(request)
                    }
                    OperationProgress::Complete(_) => {
                        request.status_request = None;
                        debug!("sdio: MMC HS200 status did not reach transfer state");
                        self.begin_mmc_hs200_fallback(request)
                    }
                }
            }
            SdioInitState::PollMmcHs200RollbackVoltage => {
                match self.advance_init_bus_op(request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        self.begin_mmc_hs200_fallback_clock(request)
                    }
                    Err(err) => {
                        debug!("sdio: MMC HS200 voltage rollback failed ({err:?})");
                        self.begin_mmc_hs200_fallback_clock(request)
                    }
                }
            }
            SdioInitState::PollMmcHs200RollbackClock => {
                match self.advance_init_bus_op(request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        self.prepare_mmc_hs52_or_complete(request)
                    }
                    Err(err) => {
                        debug!("sdio: MMC HS200 clock rollback failed ({err:?})");
                        self.prepare_mmc_hs52_or_complete(request)
                    }
                }
            }
            SdioInitState::PollMmcHs52Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_mmc_switch_request(switch_request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetClock(ClockSpeed::HighSpeed))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollMmcHighSpeedClock;
                            }
                            Err(_e) => {
                                debug!("sdio: host refused HighSpeed clock ({:?})", _e);
                                return self.prepare_mmc_cache_or_complete(request);
                            }
                        }
                        Ok(OperationProgress::Pending)
                    }
                    Err(_e) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: MMC HS_TIMING switch refused ({:?})", _e);
                        self.prepare_mmc_cache_or_complete(request)
                    }
                }
            }
            SdioInitState::PollMmcHighSpeedClock => {
                match self.advance_init_bus_op(request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        info!(
                            "sdio: MMC speed selected HighSpeed bus_width={:?}",
                            self.bus_width
                        );
                        self.prepare_mmc_cache_or_complete(request)
                    }
                    Err(_e) => {
                        debug!("sdio: host refused HighSpeed clock ({:?})", _e);
                        self.prepare_mmc_cache_or_complete(request)
                    }
                }
            }
            SdioInitState::PollMmcCacheEnable => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_mmc_switch_request(switch_request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        request.mmc_switch_request = None;
                        let ext_csd = request
                            .parsed_ext_csd
                            .as_mut()
                            .ok_or(Error::InvalidArgument)?;
                        ext_csd.set_cache_enabled(true);
                        info!(
                            "sdio: enabled {} KiB volatile write cache",
                            ext_csd.cache_size_kib()
                        );
                        request.state = SdioInitState::Complete;
                        Ok(OperationProgress::Pending)
                    }
                    Err(error) if cache_enable_may_fall_back(error) => {
                        request.mmc_switch_request = None;
                        debug!(
                            "sdio: MMC cache enable refused ({error:?}); cache remains disabled"
                        );
                        request.state = SdioInitState::Complete;
                        Ok(OperationProgress::Pending)
                    }
                    Err(error) => Err(error),
                }
            }
            _ => unreachable!("state dispatched to the wrong initialization phase"),
        }
    }

    fn prepare_mmc_cache_or_complete(
        &mut self,
        request: &mut SdioInitRequest<H>,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        let ext_csd = request
            .parsed_ext_csd
            .as_ref()
            .ok_or(Error::InvalidArgument)?;
        if ext_csd.cache_size_kib() == 0 {
            request.state = SdioInitState::Complete;
            return Ok(OperationProgress::Pending);
        }

        match self.submit_mmc_switch(0b11, crate::cmd::ext_csd::CACHE_CTRL as u8, 1) {
            Ok(switch_request) => {
                request.mmc_switch_request = Some(switch_request);
                request.state = SdioInitState::PollMmcCacheEnable;
                Ok(OperationProgress::Pending)
            }
            Err(error) if cache_enable_may_fall_back(error) => {
                debug!("sdio: MMC cache enable unsupported ({error:?}); cache remains disabled");
                request.state = SdioInitState::Complete;
                Ok(OperationProgress::Pending)
            }
            Err(error) => Err(error),
        }
    }

    fn prepare_mmc_hs52_or_complete(
        &mut self,
        request: &mut SdioInitRequest<H>,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        let supports_hs52 = request
            .parsed_ext_csd
            .as_ref()
            .ok_or(Error::InvalidArgument)?
            .device_type()
            .supports_hs_52();
        if !supports_hs52 {
            return self.prepare_mmc_cache_or_complete(request);
        }

        request.mmc_switch_request =
            Some(self.submit_mmc_switch(0b11, crate::cmd::ext_csd::HS_TIMING as u8, 1)?);
        request.state = SdioInitState::PollMmcHs52Switch;
        Ok(OperationProgress::Pending)
    }

    /// Start the event-driven rollback from a partially entered HS200 mode.
    ///
    /// `SdioBusOp` may require several register-state advances. Keeping the
    /// request in the init state machine preserves its ownership until the
    /// host reports completion instead of using a synchronous one-shot helper
    /// that would abort a still-pending clock or voltage transition.
    fn begin_mmc_hs200_fallback(
        &mut self,
        request: &mut SdioInitRequest<H>,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        let op = SdioBusOp::SwitchVoltage(SignalVoltage::V330);
        match self.host.submit_bus_op(op) {
            Ok(bus_request) => {
                request.bus_request = Some(bus_request);
                request.active_bus_op = Some(op);
                request.state = SdioInitState::PollMmcHs200RollbackVoltage;
                Ok(OperationProgress::Pending)
            }
            Err(err) => {
                debug!("sdio: cannot start MMC HS200 voltage rollback ({err:?})");
                self.begin_mmc_hs200_fallback_clock(request)
            }
        }
    }

    fn begin_mmc_hs200_fallback_clock(
        &mut self,
        request: &mut SdioInitRequest<H>,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        let op = SdioBusOp::SetClock(ClockSpeed::Default);
        match self.host.submit_bus_op(op) {
            Ok(bus_request) => {
                request.bus_request = Some(bus_request);
                request.active_bus_op = Some(op);
                request.state = SdioInitState::PollMmcHs200RollbackClock;
                Ok(OperationProgress::Pending)
            }
            Err(err) => {
                debug!("sdio: cannot start MMC HS200 clock rollback ({err:?})");
                self.prepare_mmc_hs52_or_complete(request)
            }
        }
    }
}

fn cache_enable_may_fall_back(error: Error) -> bool {
    matches!(
        error,
        Error::UnsupportedCommand
            | Error::Crc(_)
            | Error::CardError(crate::error::CardError::IllegalCommand)
    )
}
