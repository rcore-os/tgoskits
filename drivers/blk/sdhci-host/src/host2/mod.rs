use super::{block_path::should_try_dma, *};

mod bus;

impl sdio_host2::SdioHost for Sdhci {
    type TransactionRequest<'a>
        = TransactionRequest<'a>
    where
        Self: 'a;
    type BusRequest = BusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
    where
        Self: 'a,
    {
        self.check_not_poisoned().map_err(map_protocol_error)?;
        if !self.physical_bus_idle() {
            return Err(sdio_host2::Error::Busy);
        }
        let owner = self.host2_owner();
        let id = self.start_host2_request();
        let response = transaction.command.response;
        match transaction.data {
            None => {
                if let Err(err) = self.submit_command(&transaction.command) {
                    self.finish_host2_request(id);
                    return Err(map_protocol_error(err));
                }
                Ok(TransactionRequest::command(owner, id, response))
            }
            Some(phase) => {
                phase
                    .validate()
                    .inspect_err(|_| self.finish_host2_request(id))?;
                let block_size = u32::from(phase.block_size.get());
                let block_count = phase.block_count.get();
                let request = match phase.buffer {
                    sdio_host2::DataBuffer::Read(buf) => {
                        if !matches!(phase.direction, sdio_host2::DataDirection::Read) {
                            self.finish_host2_request(id);
                            return Err(sdio_host2::Error::InvalidArgument);
                        }
                        <Self as ProtocolSdioHost>::submit_read_data(
                            self,
                            &transaction.command,
                            buf,
                            block_size,
                            block_count,
                        )
                    }
                    sdio_host2::DataBuffer::Write(buf) => {
                        if !matches!(phase.direction, sdio_host2::DataDirection::Write) {
                            self.finish_host2_request(id);
                            return Err(sdio_host2::Error::InvalidArgument);
                        }
                        <Self as ProtocolSdioHost>::submit_write_data(
                            self,
                            &transaction.command,
                            buf,
                            block_size,
                            block_count,
                        )
                    }
                    sdio_host2::DataBuffer::Dma(_) => {
                        self.finish_host2_request(id);
                        return Err(sdio_host2::Error::InvalidArgument);
                    }
                }
                .inspect_err(|_| self.finish_host2_request(id))
                .map_err(map_protocol_error)?;
                Ok(TransactionRequest::data(owner, id, request, response))
            }
        }
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        if let Err(err) = self.check_not_poisoned() {
            return Err(sdio_host2::SubmitTransactionError::new(
                map_protocol_error(err),
                transaction,
            ));
        }
        if !matches!(
            transaction.data.as_ref().map(|data| &data.buffer),
            Some(sdio_host2::DataBuffer::Dma(_))
        ) {
            return unsafe { self.submit_transaction(transaction) }
                .map_err(sdio_host2::SubmitTransactionError::consumed);
        }
        if !self.physical_bus_idle() {
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::Busy,
                transaction,
            ));
        }

        let owner = self.host2_owner();
        let host2_id = self.start_host2_request();
        let response = transaction.command.response;
        let Some(phase) = transaction.data else {
            unreachable!("DMA transaction must contain a data phase")
        };
        let block_size = u32::from(phase.block_size.get());
        let block_count = phase.block_count.get();
        let sdio_host2::DataBuffer::Dma(buffer) = phase.buffer else {
            unreachable!("checked for DMA data buffer above")
        };
        let protocol_direction = match phase.direction {
            sdio_host2::DataDirection::Read => DataDirection::Read,
            sdio_host2::DataDirection::Write => DataDirection::Write,
            _ => {
                self.finish_host2_request(host2_id);
                let data = sdio_host2::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer: sdio_host2::DataBuffer::Dma(buffer),
                };
                return Err(sdio_host2::SubmitTransactionError::new(
                    sdio_host2::Error::Unsupported,
                    sdio_host2::Transaction::with_data(transaction.command, data),
                ));
            }
        };
        if !should_try_dma(
            &transaction.command,
            block_size,
            block_count,
            buffer.len().get(),
            protocol_direction,
        ) {
            self.finish_host2_request(host2_id);
            let tx = sdio_host2::Transaction::with_data(
                transaction.command,
                sdio_host2::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer: sdio_host2::DataBuffer::Dma(buffer),
                },
            );
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::Unsupported,
                tx,
            ));
        }
        let Some(dma) = self.dma.clone() else {
            self.finish_host2_request(host2_id);
            let data = sdio_host2::DataPhase {
                direction: phase.direction,
                block_size: phase.block_size,
                block_count: phase.block_count,
                buffer: sdio_host2::DataBuffer::Dma(buffer),
            };
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::Unsupported,
                sdio_host2::Transaction::with_data(transaction.command, data),
            ));
        };
        let mut slot = BlockRequestSlot::default();
        let submit = self.submit_prepared_adma2_data_request(
            &transaction.command,
            buffer,
            block_size,
            block_count,
            protocol_direction,
            &dma,
            &mut slot,
        );
        match submit {
            Ok(request) => {
                let id = request.id();
                let data = DataRequest {
                    id,
                    request: Some(request),
                    slot,
                    _buffer: PhantomData,
                };
                Ok(TransactionRequest::data(owner, host2_id, data, response))
            }
            Err(err) => {
                self.finish_host2_request(host2_id);
                let error = err.error;
                let buffer = err.into_buffer();
                let data = sdio_host2::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer: sdio_host2::DataBuffer::Dma(buffer),
                };
                Err(sdio_host2::SubmitTransactionError::new(
                    map_protocol_error(error),
                    sdio_host2::Transaction::with_data(transaction.command, data),
                ))
            }
        }
    }

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        self.check_host2_transaction_request(request)?;
        match request.kind {
            TransactionRequestKind::Command { response } => {
                match <Self as ProtocolSdioHost>::poll_command_response(self) {
                    Ok(sdmmc_protocol::CommandResponsePoll::Pending) => {
                        Ok(sdio_host2::RequestPoll::Pending)
                    }
                    Ok(sdmmc_protocol::CommandResponsePoll::Complete(resp)) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(_) => Ok(sdio_host2::RequestPoll::Pending),
                    Err(err) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Err(map_protocol_error(err))))
                    }
                }
            }
            TransactionRequestKind::Data { response } => {
                let Some(data) = request.data.as_mut() else {
                    let recovery = self.abort_host2_transaction_request(request).err();
                    return Ok(sdio_host2::RequestPoll::Ready(Err(
                        recovery.unwrap_or(sdio_host2::Error::InvalidArgument)
                    )));
                };
                match <Self as ProtocolSdioHost>::poll_data_request(self, data) {
                    Ok(DataCommandPoll::Pending) => Ok(sdio_host2::RequestPoll::Pending),
                    Ok(DataCommandPoll::Complete(resp)) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(_) => Ok(sdio_host2::RequestPoll::Pending),
                    Err(err) => {
                        let _ = self.abort_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Err(map_protocol_error(err))))
                    }
                }
            }
        }
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdio_host2::Error>
    where
        Self: 'a,
    {
        if request.done {
            return Ok(());
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        self.abort_host2_transaction_request(request)
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CompletedDma>
    where
        Self: 'a,
    {
        request
            .data
            .as_mut()
            .and_then(|data| data.slot.take_completed_dma())
    }

    unsafe fn submit_bus_op(
        &mut self,
        op: sdio_host2::BusOp,
    ) -> Result<Self::BusRequest, sdio_host2::Error> {
        self.check_not_poisoned().map_err(map_protocol_error)?;
        if !self.physical_bus_idle() {
            return Err(sdio_host2::Error::Busy);
        }
        let state = self.prepare_host2_bus_op(op)?;
        let owner = self.host2_owner();
        let id = self.start_host2_request();
        Ok(BusRequest::pending(owner, id, state))
    }

    fn poll_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError> {
        self.check_host2_bus_request(request)?;
        match self.poll_host2_bus_state(&mut request.state) {
            Ok(sdio_host2::RequestPoll::Pending) => Ok(sdio_host2::RequestPoll::Pending),
            Ok(sdio_host2::RequestPoll::Ready(Ok(()))) => {
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            Ok(sdio_host2::RequestPoll::Ready(Err(err))) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestPoll::Ready(Err(err)))
            }
            Err(err) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestPoll::Ready(Err(err)))
            }
        }
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
        if request.done {
            return Ok(());
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        let result = self.abort_host2_bus_state(&mut request.state);
        request.done = true;
        self.finish_host2_request(request.id);
        result
    }

    fn now_ms(&self) -> Option<u64> {
        self.timer.map(HostTimer::now_ms)
    }
}

impl Sdhci {
    fn physical_bus_idle(&self) -> bool {
        matches!(self.command_state, command::CommandState::Idle)
            && self.pending_data.is_none()
            && self.host2_active_id.is_none()
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
    ) -> Result<(), sdio_host2::PollRequestError> {
        if request.done {
            return Err(sdio_host2::PollRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::PollRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdio_host2::PollRequestError::StaleGeneration);
        }
        Ok(())
    }

    fn check_host2_bus_request(
        &self,
        request: &BusRequest,
    ) -> Result<(), sdio_host2::PollRequestError> {
        if request.done {
            return Err(sdio_host2::PollRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::PollRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdio_host2::PollRequestError::StaleGeneration);
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
