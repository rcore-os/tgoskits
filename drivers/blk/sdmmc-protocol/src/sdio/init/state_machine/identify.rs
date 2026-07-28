use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_identification<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        const MMC_HCS: u32 = 1 << 30;
        const MMC_VOLTAGE_MASK: u32 = 0x00FF_8000;
        const MMC_ACCESS_MODE_MASK: u32 = 0x6000_0000;

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
            SdioInitState::PollResetHost => match self.poll_init_bus_op(request)? {
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
            SdioInitState::PollPowerOn => match self.poll_init_bus_op(request)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(()) => {
                    request.state = SdioInitState::ResetVoltage;
                    request.needs_pace = true;
                    Ok(OperationPoll::Pending)
                }
            },
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
            SdioInitState::PollResetVoltage => match self.poll_init_bus_op(request)? {
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
            SdioInitState::ResetClock => self.poll_init_bus_op_then(request, |driver, request| {
                driver.submit_init_bus_op(
                    request,
                    SdioBusOp::SetClock(ClockSpeed::Identification),
                    SdioInitState::SubmitCmd0,
                )
            }),
            SdioInitState::SubmitCmd0 => self.poll_init_bus_op_then(request, |_driver, request| {
                request.state = SdioInitState::PostIdentificationClockDelay;
                request.needs_pace = true;
                Ok(OperationPoll::Pending)
            }),
            SdioInitState::PostIdentificationClockDelay => {
                debug!("sdio: CMD0 reset");
                self.host.submit_command(&crate::cmd::CMD0)?;
                request.state = SdioInitState::PollCmd0;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollCmd0 => match self.host.poll_command_response()? {
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
            SdioInitState::PollCmd8 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(Response::R7(resp))) => {
                    request.sd_v2 = resp.verify(0x01, 0xAA);
                    debug!("sdio: CMD8 sd_v2={}", request.sd_v2);
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdioInitState::PollAcmd41Cmd55;
                    Ok(OperationPoll::Pending)
                }
                Ok(CommandResponsePoll::Complete(_))
                | Err(Error::Timeout(_))
                | Err(Error::BadResponse(_))
                | Err(Error::Crc(_)) => {
                    request.sd_v2 = false;
                    debug!("sdio: CMD8 sd_v2=false");
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdioInitState::PollAcmd41Cmd55;
                    Ok(OperationPoll::Pending)
                }
                Err(e) => Err(e),
            },
            SdioInitState::PollAcmd41Cmd55 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(_)) => {
                    let acmd41 = crate::cmd::cmd41_with_s18r(request.sd_v2, 0xFF8000, true);
                    self.host.submit_command(&acmd41)?;
                    request.state = SdioInitState::PollAcmd41;
                    Ok(OperationPoll::Pending)
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
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollAcmd41 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(Response::R3(ocr))) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Sd);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Sd;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Sd, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        let elapsed_exceeded =
                            power_up_deadline_passed(&self.host, request.acmd41_started_ms);
                        if request.acmd41_polls >= SdioInitTiming::MAX_POLLS || elapsed_exceeded {
                            if !request.preference.allows_mmc_fallback() {
                                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 41)));
                            }
                            warn!(
                                "sdio: ACMD41 timed out after {} polls (~{} ms at the recommended \
                                 cadence), trying MMC CMD1",
                                request.acmd41_polls,
                                request.acmd41_polls * SdioInitTiming::POLL_TICK_MS_HINT,
                            );
                            self.host.submit_command(&crate::cmd::cmd1(0))?;
                            request.state = SdioInitState::PollMmcInitial;
                            return Ok(OperationPoll::Pending);
                        }
                        if request.acmd41_started_ms.is_none() {
                            request.acmd41_started_ms = self.host.now_ms();
                        }
                        request.acmd41_polls = request.acmd41_polls.saturating_add(1);
                        let cmd55 = crate::cmd::cmd55(0);
                        self.host.submit_command(&cmd55)?;
                        request.state = SdioInitState::PollAcmd41Cmd55;
                        request.needs_pace = true;
                    }
                    Ok(OperationPoll::Pending)
                }
                Ok(CommandResponsePoll::Complete(_)) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 41)));
                    }
                    debug!("sdio: ACMD41 returned bad response, trying MMC CMD1");
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
                Err(_sd_err) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(_sd_err);
                    }
                    debug!("sdio: ACMD41 failed ({:?}), trying MMC CMD1", _sd_err);
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcInitial => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
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
                        request.state = SdioInitState::PollMmcReady;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::PollMmcReady => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        let elapsed_exceeded =
                            power_up_deadline_passed(&self.host, request.mmc_started_ms);
                        if request.mmc_polls >= SdioInitTiming::MAX_POLLS || elapsed_exceeded {
                            warn!(
                                "sdio: CMD1 timed out after {} polls (~{} ms at the recommended \
                                 cadence)",
                                request.mmc_polls,
                                request.mmc_polls * SdioInitTiming::POLL_TICK_MS_HINT,
                            );
                            return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 1)));
                        }
                        if request.mmc_started_ms.is_none() {
                            request.mmc_started_ms = self.host.now_ms();
                        }
                        request.mmc_polls = request.mmc_polls.saturating_add(1);
                        let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                        self.host.submit_command(&cmd)?;
                        request.needs_pace = true;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::PollCmd2 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    if let Response::R2(raw) = response {
                        request.cid = Some(CidResponse::from_raw(raw));
                    } else {
                        request.cid = None;
                    }
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => self.host.submit_command(&crate::cmd::CMD3_SD)?,
                        CardKind::Mmc => self.host.submit_command(&crate::cmd::cmd3_mmc(1))?,
                    }
                    request.state = SdioInitState::PollCmd3;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd3 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
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
                    request.state = SdioInitState::PollCmd9;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd9 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    request.capacity_blocks = match response {
                        Response::R2(raw) => CsdResponse::from_raw(raw).capacity_blocks(),
                        _ => None,
                    };
                    info!("sdio: CSD capacity_blocks={:?}", request.capacity_blocks);
                    let cmd7 = crate::cmd::cmd7(self.rca);
                    self.host.submit_command(&cmd7)?;
                    request.state = SdioInitState::PollCmd7;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd7 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
                    self.high_capacity = ocr.ccs();
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => {
                            info!("sdio: switch SD bus width to 4-bit");
                            let cmd55 = crate::cmd::cmd55(self.rca);
                            self.host.submit_command(&cmd55)?;
                            request.state = SdioInitState::PollSdBusWidthCmd55;
                        }
                        CardKind::Mmc => {
                            request.state = SdioInitState::FinishCardSetup;
                        }
                    }
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollSdBusWidthCmd55 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let acmd6 = Command::new(6, sd_acmd6_arg(BusWidth::Bit4)?, ResponseType::R1);
                    self.host.submit_command(&acmd6)?;
                    request.state = SdioInitState::PollSdBusWidthAcmd6;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollSdBusWidthAcmd6 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => self.submit_init_bus_op(
                    request,
                    SdioBusOp::SetBusWidth(BusWidth::Bit4),
                    SdioInitState::PollSdHostBusWidth,
                ),
            },
            SdioInitState::PollSdHostBusWidth => {
                self.poll_init_bus_op_then(request, |driver, request| {
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
                self.poll_init_bus_op_then(request, |driver, request| {
                    if driver.sd_speed_selection_enabled {
                        request.state = SdioInitState::PrepareSdSpeed;
                    } else {
                        debug!("sdio: SD speed selection disabled; staying at default speed");
                        request.state = SdioInitState::Complete;
                    }
                    Ok(OperationPoll::Pending)
                })
            }
            _ => unreachable!("state dispatched to the wrong initialization phase"),
        }
    }
}
