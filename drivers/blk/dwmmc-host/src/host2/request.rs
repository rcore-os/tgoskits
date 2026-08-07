use super::*;
use crate::dma::DmaDataTransfer;

pub struct DataRequest<'a> {
    pub(super) id: RequestId,
    pub(super) request: Option<BlockRequest>,
    pub(super) slot: BlockRequestSlot,
    pub(super) _buffer: PhantomData<&'a [u8]>,
}

pub struct TransactionRequest<'a> {
    pub(super) owner: usize,
    pub(super) id: u64,
    pub(crate) done: bool,
    pub(super) acknowledged_irq: bool,
    pub(super) kind: TransactionRequestKind,
    pub(super) data: Option<DataRequest<'a>>,
}

pub(super) enum TransactionRequestKind {
    Command { response: sdio_host2::ResponseType },
    Data { response: sdio_host2::ResponseType },
}

impl<'a> TransactionRequest<'a> {
    pub(crate) fn command(owner: usize, id: u64, response: sdio_host2::ResponseType) -> Self {
        Self {
            owner,
            id,
            done: false,
            acknowledged_irq: false,
            kind: TransactionRequestKind::Command { response },
            data: None,
        }
    }

    pub(super) fn data(
        owner: usize,
        id: u64,
        request: DataRequest<'a>,
        response: sdio_host2::ResponseType,
    ) -> Self {
        Self {
            owner,
            id,
            done: false,
            acknowledged_irq: false,
            kind: TransactionRequestKind::Data { response },
            data: Some(request),
        }
    }
}

impl DwMmc {
    pub(super) fn submit_borrowed_read_data<'a>(
        &mut self,
        command: &Command,
        buffer: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<DataRequest<'a>, Error> {
        self.submit_borrowed_data(
            command,
            NonNull::new(buffer.as_mut_ptr()).ok_or(Error::InvalidArgument)?,
            buffer.len(),
            block_size,
            block_count,
            DataDirection::Read,
        )
    }

    pub(super) fn submit_borrowed_write_data<'a>(
        &mut self,
        command: &Command,
        buffer: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<DataRequest<'a>, Error> {
        self.submit_borrowed_data(
            command,
            NonNull::new(buffer.as_ptr().cast_mut()).ok_or(Error::InvalidArgument)?,
            buffer.len(),
            block_size,
            block_count,
            DataDirection::Write,
        )
    }

    fn submit_borrowed_data<'a>(
        &mut self,
        command: &Command,
        buffer: NonNull<u8>,
        len: usize,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
    ) -> Result<DataRequest<'a>, Error> {
        let transfer = dma_transfer_for_protocol(command, block_size, block_count, len, direction)
            .ok_or(Error::UnsupportedCommand)?;
        let dma = self.dma.take().ok_or(Error::UnsupportedCommand)?;
        let mut slot = BlockRequestSlot::default();
        let result = self.submit_dma_data(transfer, buffer, &dma, &mut slot);
        self.dma = Some(dma);
        let request = result?;
        let id = request.id();
        Ok(DataRequest {
            id,
            request: Some(request),
            slot,
            _buffer: PhantomData,
        })
    }

    pub(crate) fn command_needs_register_retry(&self) -> bool {
        matches!(
            self.command_state,
            command::CommandState::WaitingInhibit { .. }
                | command::CommandState::WaitingStart { .. }
                | command::CommandState::WaitingBusy { .. }
        )
    }

    pub(super) fn pending_transaction_progress(
        &self,
    ) -> sdio_host2::RequestProgress<sdio_host2::RawResponse> {
        if self.command_needs_register_retry() {
            sdio_host2::RequestProgress::RegisterPending {
                retry_after: DWMMC_REGISTER_RETRY_DELAY,
            }
        } else {
            sdio_host2::RequestProgress::WaitingForIrq
        }
    }

    pub(super) fn physical_bus_idle(&self) -> bool {
        matches!(self.command_state, command::CommandState::Idle)
            && self.pending_data.is_none()
            && self.data_blocks_remaining == 0
            && self.host2_active_id.is_none()
    }

    pub(super) fn start_host2_request(&mut self) -> u64 {
        let id = self.host2_next_id;
        self.host2_next_id = self.host2_next_id.wrapping_add(1);
        self.host2_active_id = Some(id);
        id
    }

    pub(crate) fn host2_owner(&self) -> usize {
        self.base_addr
    }

    pub(super) fn finish_host2_request(&mut self, id: u64) {
        if self.host2_active_id == Some(id) {
            self.host2_active_id = None;
        }
    }

    pub(super) fn check_host2_transaction_request(
        &self,
        request: &TransactionRequest<'_>,
    ) -> Result<(), sdio_host2::AdvanceRequestError> {
        if request.done {
            return Err(sdio_host2::AdvanceRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::AdvanceRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdio_host2::AdvanceRequestError::StaleGeneration);
        }
        Ok(())
    }

    pub(super) fn complete_host2_transaction_request(
        &mut self,
        request: &mut TransactionRequest<'_>,
    ) {
        request.done = true;
        self.finish_host2_request(request.id);
    }
    pub(super) fn abort_host2_transaction_request(
        &mut self,
        request: &mut TransactionRequest<'_>,
    ) -> Result<(), sdio_host2::Error> {
        let result = if let Some(data) = request.data.as_mut() {
            if let Some(active) = data.request.take() {
                let id = active.id();
                let mut pending = Some(active);
                self.abort_block_request_response(&mut pending, id, &mut data.slot)
                    .map_err(map_protocol_error)
            } else {
                Ok(())
            }
        } else {
            self.abort_command().map_err(map_protocol_error)
        };
        request.done = true;
        self.finish_host2_request(request.id);
        result
    }
}

pub(super) fn dma_transfer_for_protocol(
    cmd: &Command,
    block_size: u32,
    block_count: u32,
    len: usize,
    direction: DataDirection,
) -> Option<DmaDataTransfer> {
    DmaDataTransfer::for_protocol(cmd, block_size, block_count, len, direction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_dma_accepts_sd_switch_function_data_shape() {
        let command = sdmmc_protocol::cmd::cmd6_sd_access_mode(false, 0);

        assert!(dma_transfer_for_protocol(&command, 64, 1, 64, DataDirection::Read,).is_some());
    }

    #[test]
    fn owned_dma_accepts_mmc_ext_csd_data_shape() {
        assert!(
            dma_transfer_for_protocol(
                &sdmmc_protocol::cmd::CMD8_MMC,
                512,
                1,
                512,
                DataDirection::Read,
            )
            .is_some()
        );
    }

    #[test]
    fn owned_dma_rejects_ambiguous_or_mismatched_data_shapes() {
        let sd_switch = sdmmc_protocol::cmd::cmd6_sd_access_mode(false, 0);
        let sd_if_cond = sdmmc_protocol::cmd::cmd8(1, 0xaa);

        assert!(dma_transfer_for_protocol(&sd_switch, 64, 1, 64, DataDirection::Write).is_none());
        assert!(dma_transfer_for_protocol(&sd_switch, 512, 1, 512, DataDirection::Read).is_none());
        assert!(dma_transfer_for_protocol(&sd_if_cond, 512, 1, 512, DataDirection::Read).is_none());
        assert!(
            dma_transfer_for_protocol(
                &sdmmc_protocol::cmd::cmd17(0),
                512,
                2,
                1024,
                DataDirection::Read,
            )
            .is_none()
        );
    }
}
