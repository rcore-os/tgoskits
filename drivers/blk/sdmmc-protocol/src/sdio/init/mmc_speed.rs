use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_mmc_speed_state<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match request.state {
            SdioInitState::PollMmcHs200VoltageSwitch => {
                match self.poll_init_bus_op(request, now_ns) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        let switch_request = self.submit_mmc_switch(
                            now_ns,
                            0b11,
                            crate::cmd::ext_csd::HS_TIMING as u8,
                            0x02,
                        )?;
                        request.mmc_switch_request = Some(switch_request);
                        request.state = SdioInitState::PollMmcHs200Switch;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        warn!("sdio: HS200 voltage transition failed closed: {:?}", err);
                        Err(err)
                    }
                }
            }
            SdioInitState::PollMmcHs200Switch => match self.poll_init_mmc_switch(request, now_ns) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    request.mmc_switch_request = None;
                    let bus_request = self
                        .host
                        .submit_bus_op(SdioBusOp::SetClock(ClockSpeed::Hs200))?;
                    request.bus_request = Some(bus_request);
                    request.state = SdioInitState::PollMmcHs200Clock;
                    Ok(OperationPoll::Pending)
                }
                Err(err) => {
                    request.mmc_switch_request = None;
                    warn!("sdio: MMC HS200 switch failed closed: {:?}", err);
                    Err(err)
                }
            },
            SdioInitState::PollMmcHs200Clock => match self.poll_init_bus_op(request, now_ns) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    let block_size = self.mmc_tuning_block_size()?;
                    let bus_request = self.host.submit_bus_op(SdioBusOp::ExecuteTuning {
                        cmd_index: 21,
                        block_size,
                    })?;
                    request.bus_request = Some(bus_request);
                    request.state = SdioInitState::PollMmcHs200Tuning;
                    Ok(OperationPoll::Pending)
                }
                Err(err) => Err(err),
            },
            SdioInitState::PollMmcHs200Tuning => match self.poll_init_bus_op(request, now_ns) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    let status_request = self.submit_status()?;
                    request.status_request = Some(status_request);
                    request.state = SdioInitState::PollMmcHs200Status;
                    Ok(OperationPoll::Pending)
                }
                Err(err) => Err(err),
            },
            SdioInitState::PollMmcHs200Status => match self.poll_init_status(request, now_ns)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(CardState::Transfer) => {
                    request.status_request = None;
                    self.clock = ClockSpeed::Hs200;
                    info!("sdio: HS200 entry succeeded");
                    request.state = SdioInitState::Complete;
                    Ok(OperationPoll::Pending)
                }
                OperationPoll::Complete(_) => {
                    request.status_request = None;
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 13)))
                }
            },
            SdioInitState::PollMmcHs52Switch => match self.poll_init_mmc_switch(request, now_ns) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    request.mmc_switch_request = None;
                    let bus_request = self
                        .host
                        .submit_bus_op(SdioBusOp::SetClock(ClockSpeed::HighSpeed))?;
                    request.bus_request = Some(bus_request);
                    request.state = SdioInitState::PollMmcHighSpeedClock;
                    Ok(OperationPoll::Pending)
                }
                Err(_e) => {
                    request.mmc_switch_request = None;
                    debug!("sdio: MMC HS_TIMING switch refused ({:?})", _e);
                    request.state = SdioInitState::Complete;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcHighSpeedClock => match self.poll_init_bus_op(request, now_ns) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    self.clock = ClockSpeed::HighSpeed;
                    info!(
                        "sdio: MMC speed selected HighSpeed bus_width={:?}",
                        self.bus_width
                    );
                    request.state = SdioInitState::Complete;
                    Ok(OperationPoll::Pending)
                }
                Err(err) => Err(err),
            },
            _ => Err(Error::InvalidArgument),
        }
    }
}
