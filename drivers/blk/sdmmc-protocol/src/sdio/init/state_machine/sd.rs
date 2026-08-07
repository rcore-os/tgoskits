use super::*;

impl<H: SdioIrqHost> SdioSdmmc<H> {
    pub(super) fn advance_sd_speed_setup(
        &mut self,
        request: &mut SdioInitRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        match request.state {
            SdioInitState::PrepareSdSpeed => self.submit_sd_speed_check(request),
            SdioInitState::PollSdSwitchFunctionCheck => {
                let switch_request = request
                    .switch_function_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_switch_function_request(switch_request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        let status = finish_switch_function(request)?;
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
                        let _ = finish_switch_function(request)?;
                        warn!("sdio: SD speed selection skipped ({:?})", err);
                        request.state = SdioInitState::Complete;
                        Ok(OperationProgress::Pending)
                    }
                }
            }
            SdioInitState::PollSdVoltageSwitch => {
                let cmd = request
                    .command_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.advance_command_request(cmd, cause) {
                    Ok(CommandResponseProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(CommandResponseProgress::Complete(_)) => {
                        request.command_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SwitchVoltage(SignalVoltage::V180))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollSdSignalVoltage;
                                Ok(OperationProgress::Pending)
                            }
                            Err(err) => {
                                warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                let status = current_switch_status(request)?;
                                submit_next_sd_access_mode(self, request, status)
                            }
                        }
                    }
                    Err(err) => {
                        request.command_request = None;
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        let status = current_switch_status(request)?;
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdSignalVoltage => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.advance_init_bus_op(request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        submit_sd_access_mode_switch(self, request, mode)
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        let status = current_switch_status(request)?;
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdSetAccessMode => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                let switch_request = request
                    .switch_function_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_switch_function_request(switch_request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        let status = finish_switch_function(request)?;
                        if status.selected_function(1) != mode.function() {
                            warn!("sdio: SD {} failed (function mismatch)", mode.name());
                            submit_next_sd_access_mode(self, request, status)
                        } else {
                            match self.host.submit_bus_op(SdioBusOp::SetClock(mode.clock())) {
                                Ok(bus_request) => {
                                    request.bus_request = Some(bus_request);
                                    request.state = SdioInitState::PollSdClock;
                                    Ok(OperationProgress::Pending)
                                }
                                Err(err) => {
                                    warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                    submit_next_sd_access_mode(self, request, status)
                                }
                            }
                        }
                    }
                    Err(err) => {
                        let status = finish_switch_function(request)?;
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdClock => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.advance_init_bus_op(request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        if matches!(mode, SdAccessMode::Sdr50 | SdAccessMode::Sdr104) {
                            let block_size = self.sd_tuning_block_size()?;
                            match self.host.submit_bus_op(SdioBusOp::ExecuteTuning {
                                cmd_index: 19,
                                block_size,
                            }) {
                                Ok(bus_request) => {
                                    request.bus_request = Some(bus_request);
                                    request.state = SdioInitState::PollSdTuning;
                                    Ok(OperationProgress::Pending)
                                }
                                Err(err) => {
                                    warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                    let status = current_switch_status(request)?;
                                    submit_next_sd_access_mode(self, request, status)
                                }
                            }
                        } else {
                            let status_request = self.submit_status()?;
                            request.status_request = Some(status_request);
                            request.state = SdioInitState::PollSdStatus;
                            Ok(OperationProgress::Pending)
                        }
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        let status = current_switch_status(request)?;
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdTuning => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.advance_init_bus_op(request, cause) {
                    Ok(OperationProgress::Pending) => Ok(OperationProgress::Pending),
                    Ok(OperationProgress::Complete(())) => {
                        let status_request = self.submit_status()?;
                        request.status_request = Some(status_request);
                        request.state = SdioInitState::PollSdStatus;
                        Ok(OperationProgress::Pending)
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        let status = current_switch_status(request)?;
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdStatus => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                let status_request = request
                    .status_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.advance_status_request(status_request, cause)? {
                    OperationProgress::Pending => Ok(OperationProgress::Pending),
                    OperationProgress::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        info!("sdio: SD speed selected {:?}", mode.clock());
                        request.state = SdioInitState::Complete;
                        Ok(OperationProgress::Pending)
                    }
                    OperationProgress::Complete(_) => {
                        request.status_request = None;
                        warn!("sdio: SD {} failed (bad status)", mode.name());
                        let status = current_switch_status(request)?;
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            _ => unreachable!("state dispatched to the wrong initialization phase"),
        }
    }

    fn submit_sd_speed_check(
        &mut self,
        request: &mut SdioInitRequest<H>,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        match submit_switch_function_owned(
            self,
            request,
            &crate::cmd::cmd6_sd_access_mode(false, 0),
            SdioInitState::PollSdSwitchFunctionCheck,
        ) {
            Err(Error::UnsupportedCommand) => {
                warn!("sdio: host does not support SD CMD6; staying at default speed");
                request.state = SdioInitState::Complete;
                Ok(OperationProgress::Pending)
            }
            result => result,
        }
    }
}
