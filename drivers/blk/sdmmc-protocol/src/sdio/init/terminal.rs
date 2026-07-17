use super::*;

impl<H: SdioHost> SdioSdmmc<H> {
    pub(super) fn poll_terminal_state<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match request.state {
            SdioInitState::Complete => {
                let kind = request.kind.ok_or(Error::InvalidArgument)?;
                let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
                let link = self.link();
                if !link.is_operational() {
                    return Err(Error::InvalidArgument);
                }
                let ext_csd_timing = request.parsed_ext_csd.as_ref().map(|csd| csd.timing());
                let ext_csd_bus_width = request.parsed_ext_csd.as_ref().map(|csd| csd.bus_width());
                info!(
                    "sdio: init done kind={:?} sd_v2={} high_capacity={} rca={:#x} ocr={:#x} \
                     host_bus_width={:?} host_clock={:?} ext_csd_bus_width={:?} \
                     ext_csd_timing={:?}",
                    kind,
                    request.sd_v2,
                    self.high_capacity,
                    self.rca,
                    ocr.raw,
                    self.bus_width,
                    self.clock,
                    ext_csd_bus_width,
                    ext_csd_timing
                );
                Ok(OperationPoll::Complete(CardInfo {
                    kind,
                    sd_v2: request.sd_v2,
                    high_capacity: self.high_capacity,
                    ocr: ocr.raw,
                    rca: self.rca,
                    link,
                    capacity_blocks: request.capacity_blocks,
                    cid: request.cid,
                    ext_csd: request.parsed_ext_csd.take(),
                }))
            }
            SdioInitState::Failed => Err(request.terminal_error.unwrap_or(Error::InvalidArgument)),
            _ => Err(Error::InvalidArgument),
        }
    }
}
