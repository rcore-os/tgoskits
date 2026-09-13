use super::*;

impl<H: SdMmcIrqHost> SdMmcCard<H> {
    pub(super) fn advance_identification(
        &mut self,
        request: &mut SdMmcInitRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<CardInfo>, Error> {
        const MMC_HCS: u32 = 1 << 30;
        const MMC_VOLTAGE_MASK: u32 = 0x00FF_8000;
        const MMC_ACCESS_MODE_MASK: u32 = 0x6000_0000;

        match request.state {
            SdMmcInitState::ResetHost => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdMmcBusOp::ResetAll,
                    SdMmcInitState::PollResetHost,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support reset bus op");
                        request.state = SdMmcInitState::PowerOn;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationProgress::Pending)
            }
            SdMmcInitState::PollResetHost => match self.advance_init_bus_op(request, cause)? {
                OperationProgress::Pending => Ok(OperationProgress::Pending),
                OperationProgress::Complete(()) => {
                    request.state = SdMmcInitState::PowerOn;
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::PowerOn => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdMmcBusOp::PowerOn,
                    SdMmcInitState::PollPowerOn,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support power-on bus op");
                        request.state = SdMmcInitState::ResetVoltage;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationProgress::Pending)
            }
            SdMmcInitState::PollPowerOn => match self.advance_init_bus_op(request, cause)? {
                OperationProgress::Pending => Ok(OperationProgress::Pending),
                OperationProgress::Complete(()) => {
                    request.state = SdMmcInitState::ResetVoltage;
                    request.needs_pace = true;
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::ResetVoltage => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdMmcBusOp::SwitchVoltage(SignalVoltage::V330),
                    SdMmcInitState::PollResetVoltage,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support voltage reset");
                        request.state = SdMmcInitState::ResetBusWidth;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationProgress::Pending)
            }
            SdMmcInitState::PollResetVoltage => match self.advance_init_bus_op(request, cause)? {
                OperationProgress::Pending => Ok(OperationProgress::Pending),
                OperationProgress::Complete(()) => self.submit_init_bus_op(
                    request,
                    SdMmcBusOp::SetBusWidth(BusWidth::Bit1),
                    SdMmcInitState::ResetClock,
                ),
            },
            SdMmcInitState::ResetBusWidth => self.submit_init_bus_op(
                request,
                SdMmcBusOp::SetBusWidth(BusWidth::Bit1),
                SdMmcInitState::ResetClock,
            ),
            SdMmcInitState::ResetClock => {
                self.advance_init_bus_op_then(request, cause, |driver, request| {
                    driver.submit_init_bus_op(
                        request,
                        SdMmcBusOp::SetClock(ClockSpeed::Identification),
                        SdMmcInitState::SubmitCmd0,
                    )
                })
            }
            SdMmcInitState::SubmitCmd0 => {
                self.advance_init_bus_op_then(request, cause, |_driver, request| {
                    request.state = SdMmcInitState::PostIdentificationClockDelay;
                    request.needs_pace = true;
                    Ok(OperationProgress::Pending)
                })
            }
            SdMmcInitState::PostIdentificationClockDelay => {
                debug!("sdio: CMD0 reset");
                self.host.submit_command(&crate::cmd::CMD0)?;
                request.state = SdMmcInitState::PollCmd0;
                Ok(OperationProgress::Pending)
            }
            SdMmcInitState::PollCmd0 => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(_) => {
                    if request.preference.starts_with_sd() {
                        let cmd = crate::cmd::cmd8(0x01, 0xAA);
                        self.host.submit_command(&cmd)?;
                        request.state = SdMmcInitState::PollCmd8;
                    } else {
                        debug!("sdio: MMC-first init, trying CMD1");
                        self.host.submit_command(&crate::cmd::cmd1(0))?;
                        request.state = SdMmcInitState::PollMmcInitial;
                    }
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::PollCmd8 => match self.host.advance_command_response(cause) {
                Ok(CommandResponseProgress::Pending) => Ok(OperationProgress::Pending),
                Ok(CommandResponseProgress::Complete(Response::R7(resp))) => {
                    request.sd_v2 = resp.verify(0x01, 0xAA);
                    debug!("sdio: CMD8 sd_v2={}", request.sd_v2);
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdMmcInitState::PollAcmd41Cmd55;
                    Ok(OperationProgress::Pending)
                }
                Ok(CommandResponseProgress::Complete(_))
                | Err(Error::Timeout(_))
                | Err(Error::BadResponse(_))
                | Err(Error::Crc(_)) => {
                    request.sd_v2 = false;
                    debug!("sdio: CMD8 sd_v2=false");
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdMmcInitState::PollAcmd41Cmd55;
                    Ok(OperationProgress::Pending)
                }
                Err(e) => Err(e),
            },
            SdMmcInitState::PollAcmd41Cmd55 => match self.host.advance_command_response(cause) {
                Ok(CommandResponseProgress::Pending) => Ok(OperationProgress::Pending),
                Ok(CommandResponseProgress::Complete(_)) => {
                    let acmd41 = crate::cmd::cmd41_with_s18r(request.sd_v2, 0xFF8000, true);
                    self.host.submit_command(&acmd41)?;
                    request.state = SdMmcInitState::PollAcmd41;
                    Ok(OperationProgress::Pending)
                }
                Err(_sd_err) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(_sd_err);
                    }
                    debug!(
                        "sdio: ACMD41 prologue failed ({:?}), trying MMC CMD1",
                        _sd_err
                    );
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdMmcInitState::PollMmcInitial;
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::PollAcmd41 => match self.host.advance_command_response(cause) {
                Ok(CommandResponseProgress::Pending) => Ok(OperationProgress::Pending),
                Ok(CommandResponseProgress::Complete(Response::R3(ocr))) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Sd);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Sd;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Sd, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdMmcInitState::PollCmd2;
                    } else {
                        let elapsed_exceeded =
                            power_up_deadline_passed(self.host.inner(), request.acmd41_started_ms);
                        if request.acmd41_polls >= SdMmcInitTiming::MAX_POLLS || elapsed_exceeded {
                            if !request.preference.allows_mmc_fallback() {
                                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 41)));
                            }
                            warn!(
                                "sdio: ACMD41 timed out after {} polls (~{} ms at the recommended \
                                 cadence), trying MMC CMD1",
                                request.acmd41_polls,
                                request.acmd41_polls * SdMmcInitTiming::POLL_TICK_MS_HINT,
                            );
                            self.host.submit_command(&crate::cmd::cmd1(0))?;
                            request.state = SdMmcInitState::PollMmcInitial;
                            return Ok(OperationProgress::Pending);
                        }
                        if request.acmd41_started_ms.is_none() {
                            request.acmd41_started_ms = self.host.now_ms();
                        }
                        request.acmd41_polls = request.acmd41_polls.saturating_add(1);
                        request.state = SdMmcInitState::SubmitAcmd41Retry;
                        request.needs_pace = true;
                    }
                    Ok(OperationProgress::Pending)
                }
                Ok(CommandResponseProgress::Complete(_)) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 41)));
                    }
                    debug!("sdio: ACMD41 returned bad response, trying MMC CMD1");
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdMmcInitState::PollMmcInitial;
                    Ok(OperationProgress::Pending)
                }
                Err(_sd_err) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(_sd_err);
                    }
                    debug!("sdio: ACMD41 failed ({:?}), trying MMC CMD1", _sd_err);
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdMmcInitState::PollMmcInitial;
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::SubmitAcmd41Retry => {
                let cmd55 = crate::cmd::cmd55(0);
                self.host.submit_command(&cmd55)?;
                request.state = SdMmcInitState::PollAcmd41Cmd55;
                Ok(OperationProgress::Pending)
            }
            SdMmcInitState::PollMmcInitial => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdMmcInitState::PollCmd2;
                    } else {
                        let voltage = ocr.raw & MMC_VOLTAGE_MASK;
                        let voltage = if voltage == 0 {
                            MMC_VOLTAGE_MASK
                        } else {
                            voltage
                        };
                        request.mmc_ocr_arg = MMC_HCS | voltage | (ocr.raw & MMC_ACCESS_MODE_MASK);
                        let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                        self.host.submit_command(&cmd)?;
                        request.state = SdMmcInitState::PollMmcReady;
                    }
                    Ok(OperationProgress::Pending)
                }
                CommandResponseProgress::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdMmcInitState::PollMmcReady => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdMmcInitState::PollCmd2;
                    } else {
                        let elapsed_exceeded =
                            power_up_deadline_passed(self.host.inner(), request.mmc_started_ms);
                        if request.mmc_polls >= SdMmcInitTiming::MAX_POLLS || elapsed_exceeded {
                            warn!(
                                "sdio: controller={} media=MMC preference={:?} CMD1 timed out \
                                 after {} polls (~{} ms at the recommended cadence)",
                                self.diagnostic_identity().unwrap_or("unidentified"),
                                request.preference,
                                request.mmc_polls,
                                request.mmc_polls * SdMmcInitTiming::POLL_TICK_MS_HINT,
                            );
                            return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 1)));
                        }
                        if request.mmc_started_ms.is_none() {
                            request.mmc_started_ms = self.host.now_ms();
                        }
                        request.mmc_polls = request.mmc_polls.saturating_add(1);
                        request.state = SdMmcInitState::SubmitMmcReadyRetry;
                        request.needs_pace = true;
                    }
                    Ok(OperationProgress::Pending)
                }
                CommandResponseProgress::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdMmcInitState::SubmitMmcReadyRetry => {
                let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                self.host.submit_command(&cmd)?;
                request.state = SdMmcInitState::PollMmcReady;
                Ok(OperationProgress::Pending)
            }
            SdMmcInitState::PollCmd2 => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(response) => {
                    if let Response::R2(raw) = response {
                        request.cid = Some(CidResponse::from_raw(raw));
                    } else {
                        request.cid = None;
                    }
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => self.host.submit_command(&crate::cmd::CMD3_SD)?,
                        CardKind::Mmc => self.host.submit_command(&crate::cmd::cmd3_mmc(1))?,
                    }
                    request.state = SdMmcInitState::PollCmd3;
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::PollCmd3 => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(response) => {
                    self.rca = match (request.kind.ok_or(Error::InvalidArgument)?, response) {
                        (CardKind::Sd, Response::R6(resp)) => resp.rca(),
                        (CardKind::Mmc, Response::R1(_)) => 1,
                        _ => {
                            return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 3)));
                        }
                    };
                    debug!("sdio: CMD3 rca={:#x}", self.rca);
                    let cmd9 = crate::cmd::cmd9(self.rca);
                    self.host.submit_command(&cmd9)?;
                    request.state = SdMmcInitState::PollCmd9;
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::PollCmd9 => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(response) => {
                    request.capacity_blocks = match response {
                        Response::R2(raw) => CsdResponse::from_raw(raw).capacity_blocks(),
                        _ => None,
                    };
                    info!("sdio: CSD capacity_blocks={:?}", request.capacity_blocks);
                    let cmd7 = crate::cmd::cmd7(self.rca);
                    self.host.submit_command(&cmd7)?;
                    request.state = SdMmcInitState::PollCmd7;
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::PollCmd7 => match self.host.advance_command_response(cause)? {
                CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                CommandResponseProgress::Complete(_) => {
                    let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
                    self.high_capacity = ocr.ccs();
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => {
                            info!("sdio: switch SD bus width to 4-bit");
                            let cmd55 = crate::cmd::cmd55(self.rca);
                            self.host.submit_command(&cmd55)?;
                            request.state = SdMmcInitState::PollSdBusWidthCmd55;
                        }
                        CardKind::Mmc => {
                            request.state = SdMmcInitState::FinishCardSetup;
                        }
                    }
                    Ok(OperationProgress::Pending)
                }
            },
            SdMmcInitState::PollSdBusWidthCmd55 => {
                match self.host.advance_command_response(cause)? {
                    CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                    CommandResponseProgress::Complete(_) => {
                        let acmd6 =
                            Command::new(6, sd_acmd6_arg(BusWidth::Bit4)?, ResponseType::R1);
                        self.host.submit_command(&acmd6)?;
                        request.state = SdMmcInitState::PollSdBusWidthAcmd6;
                        Ok(OperationProgress::Pending)
                    }
                }
            }
            SdMmcInitState::PollSdBusWidthAcmd6 => {
                match self.host.advance_command_response(cause)? {
                    CommandResponseProgress::Pending => Ok(OperationProgress::Pending),
                    CommandResponseProgress::Complete(_) => self.submit_init_bus_op(
                        request,
                        SdMmcBusOp::SetBusWidth(BusWidth::Bit4),
                        SdMmcInitState::PollSdHostBusWidth,
                    ),
                }
            }
            SdMmcInitState::PollSdHostBusWidth => {
                self.advance_init_bus_op_then(request, cause, |driver, request| {
                    driver.bus_width = BusWidth::Bit4;
                    request.state = SdMmcInitState::FinishCardSetup;
                    Ok(OperationProgress::Pending)
                })
            }
            SdMmcInitState::FinishCardSetup => {
                let kind = request.kind.ok_or(Error::InvalidArgument)?;
                match kind {
                    CardKind::Sd => self.submit_init_bus_op(
                        request,
                        SdMmcBusOp::SetClock(ClockSpeed::Default),
                        SdMmcInitState::PollSdDefaultClock,
                    ),
                    CardKind::Mmc => {
                        debug!("sdio: read MMC EXT_CSD");
                        let ext_csd = request.ext_csd_buf.take().ok_or(Error::InvalidArgument)?;
                        request.ext_csd_request = match self.submit_read_ext_csd_dma(ext_csd) {
                            Ok(ext_csd_request) => Some(ext_csd_request),
                            Err(error) => {
                                let protocol_error = error.error;
                                request.ext_csd_buf = Some(error.into_buffer().into_cpu_buffer());
                                return Err(protocol_error);
                            }
                        };
                        request.state = SdMmcInitState::PollMmcExtCsd;
                        Ok(OperationProgress::Pending)
                    }
                }
            }
            SdMmcInitState::PollSdDefaultClock => {
                self.advance_init_bus_op_then(request, cause, |driver, request| {
                    if driver.sd_speed_selection_enabled {
                        request.state = SdMmcInitState::PrepareSdSpeed;
                    } else {
                        debug!("sdio: SD speed selection disabled; staying at default speed");
                        request.state = SdMmcInitState::Complete;
                    }
                    Ok(OperationProgress::Pending)
                })
            }
            _ => unreachable!("state dispatched to the wrong initialization phase"),
        }
    }
}
