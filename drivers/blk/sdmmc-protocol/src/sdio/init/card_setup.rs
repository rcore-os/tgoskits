use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_card_setup_state<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match request.state {
            SdioInitState::PollSdBusWidthCmd55 => {
                match self.poll_init_host_command(request, now_ns)? {
                    CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                    CommandResponsePoll::Complete(_) => {
                        let acmd6 =
                            Command::new(6, sd_acmd6_arg(BusWidth::Bit4)?, ResponseType::R1);
                        self.host.submit_command(&acmd6)?;
                        request.state = SdioInitState::PollSdBusWidthAcmd6;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollSdBusWidthAcmd6 => {
                match self.poll_init_host_command(request, now_ns)? {
                    CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                    CommandResponsePoll::Complete(_) => self.submit_init_bus_op(
                        request,
                        SdioBusOp::SetBusWidth(BusWidth::Bit4),
                        SdioInitState::PollSdHostBusWidth,
                    ),
                }
            }
            SdioInitState::PollSdHostBusWidth => {
                self.poll_init_bus_op_then(request, now_ns, |driver, request| {
                    driver.bus_width = BusWidth::Bit4;
                    request.state = SdioInitState::FinishCardSetup;
                    Ok(OperationPoll::Pending)
                })
            }
            SdioInitState::FinishCardSetup => {
                let kind = request.kind.ok_or(Error::InvalidArgument)?;
                match kind {
                    CardKind::Sd => self.submit_init_bus_op(
                        request,
                        SdioBusOp::SetClock(ClockSpeed::Default),
                        SdioInitState::PollSdDefaultClock,
                    ),
                    CardKind::Mmc => {
                        debug!("sdio: read MMC EXT_CSD");
                        // SAFETY: the slot's debug_assert traps re-lending; the
                        // returned reference's lifetime is bound to the host's
                        // DataRequest via SwitchFunctionRequest/ExtCsdRequest,
                        // and we release on the Complete arm below.
                        let ext_csd = unsafe { request.ext_csd_buf.lend() };
                        request.ext_csd_request = Some(self.submit_read_ext_csd(ext_csd)?);
                        request.state = SdioInitState::PollMmcExtCsd;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollSdDefaultClock => {
                self.poll_init_bus_op_then(request, now_ns, |driver, request| {
                    driver.clock = ClockSpeed::Default;
                    if driver.sd_speed_selection_enabled {
                        request.state = SdioInitState::PrepareSdSpeed;
                    } else {
                        debug!("sdio: SD speed selection disabled; staying at default speed");
                        request.state = SdioInitState::Complete;
                    }
                    Ok(OperationPoll::Pending)
                })
            }
            SdioInitState::PollMmcExtCsd => {
                match self.poll_init_ext_csd(request, now_ns)? {
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
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit8, now_ns)
                    }
                }
            }
            SdioInitState::PollMmcBusWidth => match self.poll_init_mmc_switch(request, now_ns) {
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
                        Err(err) => handle_mmc_host_bus_width_error(self, request, err, now_ns),
                    }
                }
                Err(err) if matches!(request.current_bus_width, BusWidth::Bit8) => {
                    request.mmc_switch_request = None;
                    debug!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
                    submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit4, now_ns)
                }
                Err(err) if matches!(request.current_bus_width, BusWidth::Bit4) => {
                    request.mmc_switch_request = None;
                    debug!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
                    submit_mmc_default_clock(self, request)
                }
                Err(err) => Err(err),
            },
            SdioInitState::PollMmcHostBusWidth => {
                let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
                match self.host.poll_bus_op_at(&mut bus_request, now_ns) {
                    Ok(OperationPoll::Pending) => {
                        request.bus_wake_at_ns = self.host.bus_op_wake_at(&bus_request);
                        request.bus_request = Some(bus_request);
                        Ok(OperationPoll::Pending)
                    }
                    Ok(OperationPoll::Complete(())) => {
                        request.bus_wake_at_ns = None;
                        self.bus_width = request.current_bus_width;
                        submit_mmc_default_clock(self, request)
                    }
                    Err(err) => {
                        request.bus_wake_at_ns = None;
                        handle_mmc_host_bus_width_error(self, request, err, now_ns)
                    }
                }
            }
            SdioInitState::PollMmcDefaultClock => {
                self.poll_init_bus_op_then(request, now_ns, |driver, request| {
                    driver.clock = ClockSpeed::Default;
                    request.state = SdioInitState::PrepareMmcSpeed;
                    Ok(OperationPoll::Pending)
                })
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
                        Err(err) => return Err(err),
                    }
                }
                if dt.supports_hs_52() {
                    let switch_request = self.submit_mmc_switch(
                        now_ns,
                        0b11,
                        crate::cmd::ext_csd::HS_TIMING as u8,
                        1,
                    )?;
                    request.mmc_switch_request = Some(switch_request);
                    request.state = SdioInitState::PollMmcHs52Switch;
                } else {
                    request.state = SdioInitState::Complete;
                }
                Ok(OperationPoll::Pending)
            }
            _ => Err(Error::InvalidArgument),
        }
    }
}

fn submit_mmc_default_clock<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
) -> Result<OperationPoll<CardInfo>, Error> {
    driver.submit_init_bus_op(
        request,
        SdioBusOp::SetClock(ClockSpeed::Default),
        SdioInitState::PollMmcDefaultClock,
    )
}

fn submit_mmc_bus_width_or_continue<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    width: BusWidth,
    now_ns: u64,
) -> Result<OperationPoll<CardInfo>, Error> {
    let value: u8 = match width {
        BusWidth::Bit1 => 0,
        BusWidth::Bit4 => 1,
        BusWidth::Bit8 => 2,
        _ => return Err(Error::UnsupportedCommand),
    };
    request.current_bus_width = width;
    request.mmc_switch_request = Some(driver.submit_mmc_switch(
        now_ns,
        0b11,
        crate::cmd::ext_csd::BUS_WIDTH as u8,
        value,
    )?);
    request.state = SdioInitState::PollMmcBusWidth;
    Ok(OperationPoll::Pending)
}

fn handle_mmc_host_bus_width_error<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    err: Error,
    now_ns: u64,
) -> Result<OperationPoll<CardInfo>, Error> {
    request.bus_request = None;
    if matches!(request.current_bus_width, BusWidth::Bit8) {
        debug!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
        submit_mmc_bus_width_or_continue(driver, request, BusWidth::Bit4, now_ns)
    } else if matches!(request.current_bus_width, BusWidth::Bit4) {
        debug!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
        submit_mmc_default_clock(driver, request)
    } else {
        Err(err)
    }
}
