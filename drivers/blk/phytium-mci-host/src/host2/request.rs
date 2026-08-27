use super::*;

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
    Command { response: sdmmc_host::ResponseType },
    Data { response: sdmmc_host::ResponseType },
}

impl<'a> TransactionRequest<'a> {
    pub(crate) fn command(owner: usize, id: u64, response: sdmmc_host::ResponseType) -> Self {
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
        response: sdmmc_host::ResponseType,
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

impl PhytiumMci {
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
    ) -> sdmmc_host::RequestProgress<sdmmc_host::RawResponse> {
        if self.command_needs_register_retry() {
            register_pending()
        } else {
            sdmmc_host::RequestProgress::WaitingForIrq
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
    ) -> Result<(), sdmmc_host::AdvanceRequestError> {
        if request.done {
            return Err(sdmmc_host::AdvanceRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdmmc_host::AdvanceRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdmmc_host::AdvanceRequestError::StaleGeneration);
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
    ) -> Result<(), sdmmc_host::Error> {
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

pub(super) fn submit_borrowed_read_data<'a>(
    host: &mut PhytiumMci,
    cmd: &Command,
    buffer: &'a mut [u8],
    block_size: u32,
    block_count: u32,
) -> Result<DataRequest<'a>, Error> {
    if !supports_borrowed_block_dma(
        cmd,
        block_size,
        block_count,
        buffer.len(),
        DataDirection::Read,
    ) {
        return Err(Error::UnsupportedCommand);
    }
    let ptr = NonNull::new(buffer.as_mut_ptr()).ok_or(Error::InvalidArgument)?;
    let dma = host.dma.take().ok_or(Error::UnsupportedCommand)?;
    let mut slot = BlockRequestSlot::default();
    let result = host.submit_read_blocks(
        cmd.argument,
        ptr,
        NonZeroUsize::new(buffer.len()).ok_or(Error::InvalidArgument)?,
        &dma,
        &mut slot,
    );
    host.dma = Some(dma);
    let request = result?;
    Ok(DataRequest {
        id: request.id(),
        request: Some(request),
        slot,
        _buffer: PhantomData,
    })
}

pub(super) fn submit_borrowed_write_data<'a>(
    host: &mut PhytiumMci,
    cmd: &Command,
    buffer: &'a [u8],
    block_size: u32,
    block_count: u32,
) -> Result<DataRequest<'a>, Error> {
    if !supports_borrowed_block_dma(
        cmd,
        block_size,
        block_count,
        buffer.len(),
        DataDirection::Write,
    ) {
        return Err(Error::UnsupportedCommand);
    }
    let ptr = NonNull::new(buffer.as_ptr() as *mut u8).ok_or(Error::InvalidArgument)?;
    let dma = host.dma.take().ok_or(Error::UnsupportedCommand)?;
    let mut slot = BlockRequestSlot::default();
    let result = host.submit_write_blocks(
        cmd.argument,
        ptr,
        NonZeroUsize::new(buffer.len()).ok_or(Error::InvalidArgument)?,
        &dma,
        &mut slot,
    );
    host.dma = Some(dma);
    let request = result?;
    Ok(DataRequest {
        id: request.id(),
        request: Some(request),
        slot,
        _buffer: PhantomData,
    })
}

pub(super) fn supports_borrowed_block_dma(
    cmd: &Command,
    block_size: u32,
    block_count: u32,
    len: usize,
    direction: DataDirection,
) -> bool {
    block_size == 512
        && len == block_count as usize * 512
        && matches!(
            (direction, cmd.index),
            (DataDirection::Read, 17 | 18) | (DataDirection::Write, 24 | 25)
        )
}

pub(crate) fn supports_owned_dma_transaction(
    block_size: u32,
    block_count: u32,
    len: usize,
    direction: DataDirection,
) -> bool {
    block_size != 0
        && block_count != 0
        && usize::try_from(block_size)
            .ok()
            .and_then(|size| size.checked_mul(block_count as usize))
            == Some(len)
        && matches!(direction, DataDirection::Read | DataDirection::Write)
}
