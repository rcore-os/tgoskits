use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_bootstrap_state<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match request.state {
            SdioInitState::ResetHost => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdioBusOp::ResetAll,
                    SdioInitState::PollResetHost,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support reset bus op");
                        request.state = SdioInitState::PowerOn;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollResetHost => match self.poll_init_bus_op(request, now_ns)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(()) => {
                    request.state = SdioInitState::PowerOn;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PowerOn => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdioBusOp::PowerOn,
                    SdioInitState::PollPowerOn,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support power-on bus op");
                        request.state = SdioInitState::ResetVoltage;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollPowerOn => match self.poll_init_bus_op(request, now_ns)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(()) => {
                    request.retry_at_ns = Some(now_ns.saturating_add(INIT_RETRY_INTERVAL_NS));
                    request.state = SdioInitState::PostPowerOnDelay;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PostPowerOnDelay => {
                request.retry_at_ns = None;
                request.state = SdioInitState::ResetVoltage;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::ResetVoltage => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdioBusOp::SwitchVoltage(SignalVoltage::V330),
                    SdioInitState::PollResetVoltage,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support voltage reset");
                        request.state = SdioInitState::ResetBusWidth;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollResetVoltage => match self.poll_init_bus_op(request, now_ns)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(()) => self.submit_init_bus_op(
                    request,
                    SdioBusOp::SetBusWidth(BusWidth::Bit1),
                    SdioInitState::ResetClock,
                ),
            },
            SdioInitState::ResetBusWidth => self.submit_init_bus_op(
                request,
                SdioBusOp::SetBusWidth(BusWidth::Bit1),
                SdioInitState::ResetClock,
            ),
            SdioInitState::ResetClock => {
                self.poll_init_bus_op_then(request, now_ns, |driver, request| {
                    driver.submit_init_bus_op(
                        request,
                        SdioBusOp::SetClock(ClockSpeed::Identification),
                        SdioInitState::SubmitCmd0,
                    )
                })
            }
            SdioInitState::SubmitCmd0 => {
                self.poll_init_bus_op_then(request, now_ns, |driver, request| {
                    driver.clock = ClockSpeed::Identification;
                    request.state = SdioInitState::PostIdentificationClockDelay;
                    request.retry_at_ns = Some(now_ns.saturating_add(INIT_RETRY_INTERVAL_NS));
                    Ok(OperationPoll::Pending)
                })
            }
            SdioInitState::PostIdentificationClockDelay => {
                request.retry_at_ns = None;
                debug!("sdio: CMD0 reset");
                self.host.submit_command(&crate::cmd::CMD0)?;
                request.state = SdioInitState::PollCmd0;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollCmd0 => match self.poll_init_host_command(request, now_ns)? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    if request.preference.starts_with_sd() {
                        let cmd = crate::cmd::cmd8(0x01, 0xAA);
                        self.host.submit_command(&cmd)?;
                        request.state = SdioInitState::PollCmd8;
                    } else {
                        debug!("sdio: MMC-first init, trying CMD1");
                        self.host.submit_command(&crate::cmd::cmd1(0))?;
                        request.state = SdioInitState::PollMmcInitial;
                    }
                    Ok(OperationPoll::Pending)
                }
            },
            _ => Err(Error::InvalidArgument),
        }
    }
}
