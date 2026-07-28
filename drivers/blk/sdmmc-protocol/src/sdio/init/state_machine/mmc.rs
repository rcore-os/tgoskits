use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_mmc_setup<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match request.state {
            SdioInitState::PollMmcExtCsd => {
                let ext_request = request
                    .ext_csd_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_ext_csd_request(ext_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(()) => {
                        request.ext_csd_request = None;
                        request.ext_csd_buf.release();
                        // SAFETY: we just released the slot above; the host
                        // has finished writing the buffer (DataCommandPoll::
                        // Complete is the host's promise) and nothing else
                        // holds a reference.
                        let csd = crate::ext_csd::ExtCsd::from_bytes(unsafe {
                            *request.ext_csd_buf.peek()
                        });
                        if let Some(sectors) = csd.sector_count() {
                            request.capacity_blocks = Some(sectors as u64);
                            info!("sdio: EXT_CSD sector_count={}", sectors);
                        }
                        request.parsed_ext_csd = Some(csd);
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit8)
                    }
                }
            }
            SdioInitState::PollMmcBusWidth => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetBusWidth(request.current_bus_width))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollMmcHostBusWidth;
                                Ok(OperationPoll::Pending)
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
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => Err(err),
                }
            }
            SdioInitState::PollMmcHostBusWidth => {
                let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
                match self.host.poll_bus_op(&mut bus_request) {
                    Ok(OperationPoll::Pending) => {
                        request.bus_request = Some(bus_request);
                        Ok(OperationPoll::Pending)
                    }
                    Ok(OperationPoll::Complete(())) => {
                        self.bus_width = request.current_bus_width;
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => handle_mmc_host_bus_width_error(self, request, err),
                }
            }
            SdioInitState::PrepareMmcSpeed => {
                let Some(csd) = request.parsed_ext_csd.as_ref() else {
                    return Err(Error::InvalidArgument);
                };
                let dt = csd.device_type();
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
                            request.state = SdioInitState::PollMmcHs200VoltageSwitch;
                            return Ok(OperationPoll::Pending);
                        }
                        // The host has no way to actually drive the IO rail
                        // at 1.8 V (controllers like the rk3568 SDHCI MVP
                        // refuse here on purpose); HS200 requires 1.8 V, so
                        // skip the attempt entirely instead of leaving the
                        // controller's 1.8 V Signaling Enable bit set while
                        // running the bus at 3.3 V.
                        Err(Error::UnsupportedCommand) => {}
                        Err(err) => debug!("sdio: switch_voltage(V180) failed ({:?})", err),
                    }
                    self.rollback_to_hs_compat();
                }
                if dt.supports_hs_52() {
                    let switch_request =
                        self.submit_mmc_switch(0b11, crate::cmd::ext_csd::HS_TIMING as u8, 1)?;
                    request.mmc_switch_request = Some(switch_request);
                    request.state = SdioInitState::PollMmcHs52Switch;
                } else {
                    return self.prepare_mmc_cache_or_complete(request);
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollMmcHs200VoltageSwitch => {
                let Some(csd) = request.parsed_ext_csd.as_ref() else {
                    return Err(Error::InvalidArgument);
                };
                let supports_hs52 = csd.device_type().supports_hs_52();
                match self.poll_init_bus_op(request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        let switch_request = self.submit_mmc_switch(
                            0b11,
                            crate::cmd::ext_csd::HS_TIMING as u8,
                            0x02,
                        )?;
                        request.mmc_switch_request = Some(switch_request);
                        request.state = SdioInitState::PollMmcHs200Switch;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        debug!("sdio: switch_voltage(V180) failed ({:?})", err);
                        self.rollback_to_hs_compat();
                        if supports_hs52 {
                            let switch_request = self.submit_mmc_switch(
                                0b11,
                                crate::cmd::ext_csd::HS_TIMING as u8,
                                1,
                            )?;
                            request.mmc_switch_request = Some(switch_request);
                            request.state = SdioInitState::PollMmcHs52Switch;
                        } else {
                            return self.prepare_mmc_cache_or_complete(request);
                        }
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs200Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetClock(ClockSpeed::Hs200))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollMmcHs200Clock;
                            }
                            Err(_) => {
                                self.rollback_to_hs_compat();
                                request.state = SdioInitState::PrepareMmcSpeed;
                            }
                        }
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: MMC HS200 switch refused ({:?})", err);
                        self.rollback_to_hs_compat();
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs200Clock => match self.poll_init_bus_op(request) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    let block_size = self.mmc_tuning_block_size()?;
                    match self.host.submit_bus_op(SdioBusOp::ExecuteTuning {
                        cmd_index: 21,
                        block_size,
                    }) {
                        Ok(bus_request) => {
                            request.bus_request = Some(bus_request);
                            request.state = SdioInitState::PollMmcHs200Tuning;
                        }
                        Err(_) => {
                            self.rollback_to_hs_compat();
                            request.state = SdioInitState::PrepareMmcSpeed;
                        }
                    }
                    Ok(OperationPoll::Pending)
                }
                Err(_) => {
                    self.rollback_to_hs_compat();
                    request.state = SdioInitState::PrepareMmcSpeed;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcHs200Tuning => match self.poll_init_bus_op(request) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    let status_request = self.submit_status()?;
                    request.status_request = Some(status_request);
                    request.state = SdioInitState::PollMmcHs200Status;
                    Ok(OperationPoll::Pending)
                }
                Err(_) => {
                    self.rollback_to_hs_compat();
                    request.state = SdioInitState::PrepareMmcSpeed;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcHs200Status => {
                let status_request = request
                    .status_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_status_request(status_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        info!("sdio: HS200 entry succeeded");
                        self.prepare_mmc_cache_or_complete(request)
                    }
                    OperationPoll::Complete(_) => {
                        request.status_request = None;
                        self.rollback_to_hs_compat();
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs52Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
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
                        Ok(OperationPoll::Pending)
                    }
                    Err(_e) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: MMC HS_TIMING switch refused ({:?})", _e);
                        self.prepare_mmc_cache_or_complete(request)
                    }
                }
            }
            SdioInitState::PollMmcHighSpeedClock => match self.poll_init_bus_op(request) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
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
            },
            SdioInitState::PollMmcCacheEnable => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
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
                        Ok(OperationPoll::Pending)
                    }
                    Err(error) if cache_enable_may_fall_back(error) => {
                        request.mmc_switch_request = None;
                        debug!(
                            "sdio: MMC cache enable refused ({error:?}); cache remains disabled"
                        );
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                    Err(error) => Err(error),
                }
            }
            _ => unreachable!("state dispatched to the wrong initialization phase"),
        }
    }

    fn prepare_mmc_cache_or_complete<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        let ext_csd = request
            .parsed_ext_csd
            .as_ref()
            .ok_or(Error::InvalidArgument)?;
        if ext_csd.cache_size_kib() == 0 {
            request.state = SdioInitState::Complete;
            return Ok(OperationPoll::Pending);
        }

        match self.submit_mmc_switch(0b11, crate::cmd::ext_csd::CACHE_CTRL as u8, 1) {
            Ok(switch_request) => {
                request.mmc_switch_request = Some(switch_request);
                request.state = SdioInitState::PollMmcCacheEnable;
                Ok(OperationPoll::Pending)
            }
            Err(error) if cache_enable_may_fall_back(error) => {
                debug!("sdio: MMC cache enable unsupported ({error:?}); cache remains disabled");
                request.state = SdioInitState::Complete;
                Ok(OperationPoll::Pending)
            }
            Err(error) => Err(error),
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
