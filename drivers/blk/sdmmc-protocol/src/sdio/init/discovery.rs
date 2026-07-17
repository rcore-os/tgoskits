use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_discovery_state<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        const MMC_HCS: u32 = 1 << 30;
        const MMC_VOLTAGE_MASK: u32 = 0x00FF_8000;
        const MMC_ACCESS_MODE_MASK: u32 = 0x6000_0000;

        match request.state {
            SdioInitState::PollCmd8 => match self.poll_init_host_command(request, now_ns) {
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
            SdioInitState::PollAcmd41Cmd55 => match self.poll_init_host_command(request, now_ns) {
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
                    request.power_deadline_ns = None;
                    request.retry_at_ns = None;
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollAcmd41 => match self.poll_init_host_command(request, now_ns) {
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
                        request
                            .power_deadline_ns
                            .get_or_insert_with(|| now_ns.saturating_add(INIT_POWER_UP_TIMEOUT_NS));
                        request.retry_at_ns = Some(now_ns.saturating_add(INIT_RETRY_INTERVAL_NS));
                        request.state = SdioInitState::WaitAcmd41Retry;
                    }
                    Ok(OperationPoll::Pending)
                }
                Ok(CommandResponsePoll::Complete(_)) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 41)));
                    }
                    debug!("sdio: ACMD41 returned bad response, trying MMC CMD1");
                    request.power_deadline_ns = None;
                    request.retry_at_ns = None;
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
                Err(_sd_err) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(_sd_err);
                    }
                    debug!("sdio: ACMD41 failed ({:?}), trying MMC CMD1", _sd_err);
                    request.power_deadline_ns = None;
                    request.retry_at_ns = None;
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::WaitAcmd41Retry => {
                let deadline = request.power_deadline_ns.ok_or(Error::InvalidArgument)?;
                if now_ns >= deadline {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 41)));
                    }
                    warn!("sdio: ACMD41 power-up deadline expired, trying MMC CMD1");
                    request.power_deadline_ns = None;
                    request.retry_at_ns = None;
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    return Ok(OperationPoll::Pending);
                }
                request.retry_at_ns = None;
                self.host.submit_command(&crate::cmd::cmd55(0))?;
                request.state = SdioInitState::PollAcmd41Cmd55;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollMmcInitial => match self.poll_init_host_command(request, now_ns)? {
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
                        request
                            .power_deadline_ns
                            .get_or_insert_with(|| now_ns.saturating_add(INIT_POWER_UP_TIMEOUT_NS));
                        request.retry_at_ns = Some(now_ns.saturating_add(INIT_RETRY_INTERVAL_NS));
                        request.state = SdioInitState::WaitMmcRetry;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::PollMmcReady => match self.poll_init_host_command(request, now_ns)? {
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
                        request
                            .power_deadline_ns
                            .get_or_insert_with(|| now_ns.saturating_add(INIT_POWER_UP_TIMEOUT_NS));
                        request.retry_at_ns = Some(now_ns.saturating_add(INIT_RETRY_INTERVAL_NS));
                        request.state = SdioInitState::WaitMmcRetry;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::WaitMmcRetry => {
                let deadline = request.power_deadline_ns.ok_or(Error::InvalidArgument)?;
                if now_ns >= deadline {
                    return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 1)));
                }
                request.retry_at_ns = None;
                let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                self.host.submit_command(&cmd)?;
                request.state = SdioInitState::PollMmcReady;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollCmd2 => match self.poll_init_host_command(request, now_ns)? {
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
            SdioInitState::PollCmd3 => match self.poll_init_host_command(request, now_ns)? {
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
            SdioInitState::PollCmd9 => match self.poll_init_host_command(request, now_ns)? {
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
            SdioInitState::PollCmd7 => match self.poll_init_host_command(request, now_ns)? {
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
            _ => Err(Error::InvalidArgument),
        }
    }
}
