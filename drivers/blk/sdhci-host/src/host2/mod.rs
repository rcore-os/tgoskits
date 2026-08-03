use super::{block_path::adma2_shape_supported, *};

mod bus;

mod transaction;

impl Sdhci {
    fn submit_borrowed_read_data<'a>(
        &mut self,
        command: &Command,
        buffer: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<DataRequest<'a>, Error> {
        let address = NonNull::new(buffer.as_mut_ptr()).ok_or(Error::InvalidArgument)?;
        let (id, request, slot) = self.submit_borrowed_data(
            command,
            address,
            buffer.len(),
            block_size,
            block_count,
            DataDirection::Read,
        )?;
        Ok(DataRequest {
            id,
            request: Some(request),
            slot,
            _buffer: PhantomData,
        })
    }

    fn submit_borrowed_write_data<'a>(
        &mut self,
        command: &Command,
        buffer: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<DataRequest<'a>, Error> {
        let address = NonNull::new(buffer.as_ptr().cast_mut()).ok_or(Error::InvalidArgument)?;
        let (id, request, slot) = self.submit_borrowed_data(
            command,
            address,
            buffer.len(),
            block_size,
            block_count,
            DataDirection::Write,
        )?;
        Ok(DataRequest {
            id,
            request: Some(request),
            slot,
            _buffer: PhantomData,
        })
    }

    fn submit_borrowed_data(
        &mut self,
        command: &Command,
        buffer: NonNull<u8>,
        len: usize,
        block_size: u32,
        block_count: u32,
        direction: DataDirection,
    ) -> Result<(RequestId, BlockRequest, BlockRequestSlot), Error> {
        let mut slot = BlockRequestSlot::default();
        let request = match direction {
            DataDirection::Read => submit_read_adma2(
                self,
                command,
                buffer,
                len,
                block_size,
                block_count,
                &mut slot,
            ),
            DataDirection::Write => submit_write_adma2(
                self,
                command,
                buffer,
                len,
                block_size,
                block_count,
                &mut slot,
            ),
            _ => return Err(Error::UnsupportedCommand),
        }?;
        let id = request.id();
        Ok((id, request, slot))
    }

    fn pending_transaction_progress(&self) -> sdio_host2::RequestProgress<sdio_host2::RawResponse> {
        match self.progress_wait_kind() {
            sdmmc_protocol::sdio::HostProgressWait::Register { retry_after } => {
                sdio_host2::RequestProgress::RegisterPending { retry_after }
            }
            sdmmc_protocol::sdio::HostProgressWait::Irq => {
                sdio_host2::RequestProgress::WaitingForIrq
            }
        }
    }

    fn physical_bus_idle(&self) -> bool {
        matches!(self.command_state, command::CommandState::Idle) && self.host2_active_id.is_none()
    }

    fn start_host2_request(&mut self) -> u64 {
        let id = self.host2_next_id;
        self.host2_next_id = self.host2_next_id.wrapping_add(1);
        self.host2_active_id = Some(id);
        id
    }

    fn host2_owner(&self) -> usize {
        self.base_addr
    }

    fn finish_host2_request(&mut self, id: u64) {
        if self.host2_active_id == Some(id) {
            self.host2_active_id = None;
        }
    }

    fn check_host2_transaction_request(
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

    fn check_host2_bus_request(
        &self,
        request: &BusRequest,
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

    fn complete_host2_transaction_request(&mut self, request: &mut TransactionRequest<'_>) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    fn complete_host2_bus_request(&mut self, request: &mut BusRequest) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    fn abort_host2_transaction_request(
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

fn map_protocol_error(err: Error) -> sdio_host2::Error {
    match err {
        Error::Timeout(_) => sdio_host2::Error::Timeout,
        Error::Crc(_) => sdio_host2::Error::Crc,
        Error::NoCard => sdio_host2::Error::NoCard,
        Error::Busy => sdio_host2::Error::Busy,
        Error::UnsupportedCommand => sdio_host2::Error::Unsupported,
        Error::Misaligned => sdio_host2::Error::Misaligned,
        Error::InvalidArgument => sdio_host2::Error::InvalidArgument,
        Error::BusError(_) => sdio_host2::Error::Bus,
        Error::ReadError(_) | Error::WriteError(_) | Error::BadResponse(_) => {
            sdio_host2::Error::Bus
        }
        Error::CardError(_) | Error::CardLocked => sdio_host2::Error::Controller,
        _ => sdio_host2::Error::Controller,
    }
}
