use super::{
    request::{TransactionRequestKind, dma_transfer_for_protocol},
    *,
};

impl sdio_host2::SdioHost for DwMmc {
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
                        self.submit_borrowed_read_data(
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
                        self.submit_borrowed_write_data(
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
        if !self.card_present() {
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::NoCard,
                transaction,
            ));
        }
        if !matches!(
            transaction.data.as_ref().map(|data| &data.buffer),
            Some(sdio_host2::DataBuffer::Dma(_))
        ) {
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::Unsupported,
                transaction,
            ));
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
        let Some(transfer) = dma_transfer_for_protocol(
            &transaction.command,
            block_size,
            block_count,
            buffer.len().get(),
            protocol_direction,
        ) else {
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
        let Some(dma) = self.dma.take() else {
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
        let submit = self.submit_prepared_data(transfer, buffer, &dma, &mut slot);
        self.dma = Some(dma);
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

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: sdio_host2::ProgressCause,
    ) -> Result<sdio_host2::RequestProgress<sdio_host2::RawResponse>, sdio_host2::AdvanceRequestError>
    where
        Self: 'a,
    {
        self.check_host2_transaction_request(request)?;
        let acknowledged_irq = matches!(cause, sdio_host2::ProgressCause::AcknowledgedIrq);
        request.acknowledged_irq |= acknowledged_irq;
        if !request.acknowledged_irq && !self.command_needs_register_retry() {
            return Ok(sdio_host2::RequestProgress::WaitingForIrq);
        }
        match request.kind {
            TransactionRequestKind::Command { response } => {
                match self.advance_command_response(cause) {
                    Ok(CommandResponseProgress::Pending) => Ok(self.pending_transaction_progress()),
                    Ok(CommandResponseProgress::Complete(resp)) if request.acknowledged_irq => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestProgress::Complete(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(CommandResponseProgress::Complete(_)) => {
                        Ok(sdio_host2::RequestProgress::WaitingForIrq)
                    }
                    Err(err) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestProgress::Complete(Err(
                            map_protocol_error(err),
                        )))
                    }
                }
            }
            TransactionRequestKind::Data { response } => {
                let Some(data) = request.data.as_mut() else {
                    let recovery = self.abort_host2_transaction_request(request).err();
                    return Ok(sdio_host2::RequestProgress::Complete(Err(
                        recovery.unwrap_or(sdio_host2::Error::InvalidArgument)
                    )));
                };
                match self.advance_block_request_response(
                    &mut data.request,
                    data.id,
                    &mut data.slot,
                    cause,
                ) {
                    Ok(DataCommandProgress::Pending) => Ok(self.pending_transaction_progress()),
                    Ok(DataCommandProgress::Complete(resp)) if request.acknowledged_irq => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestProgress::Complete(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(DataCommandProgress::Complete(_)) => {
                        Ok(sdio_host2::RequestProgress::WaitingForIrq)
                    }
                    Err(err) => {
                        let _ = self.abort_host2_transaction_request(request);
                        Ok(sdio_host2::RequestProgress::Complete(Err(
                            map_protocol_error(err),
                        )))
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

    fn advance_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
        cause: sdio_host2::ProgressCause,
    ) -> Result<sdio_host2::RequestProgress<()>, sdio_host2::AdvanceRequestError> {
        self.check_host2_bus_request(request)?;
        if cause == sdio_host2::ProgressCause::AcknowledgedIrq {
            return Ok(sdio_host2::RequestProgress::RegisterPending {
                retry_after: DWMMC_REGISTER_RETRY_DELAY,
            });
        }
        match self.advance_host2_bus_state(&mut request.state) {
            Ok(sdio_host2::RequestProgress::RegisterPending { retry_after }) => {
                Ok(sdio_host2::RequestProgress::RegisterPending { retry_after })
            }
            Ok(sdio_host2::RequestProgress::WaitingForIrq) => {
                Ok(sdio_host2::RequestProgress::RegisterPending {
                    retry_after: DWMMC_REGISTER_RETRY_DELAY,
                })
            }
            Ok(sdio_host2::RequestProgress::Complete(Ok(()))) => {
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestProgress::Complete(Ok(())))
            }
            Ok(sdio_host2::RequestProgress::Complete(Err(err))) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestProgress::Complete(Err(err)))
            }
            Err(err) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestProgress::Complete(Err(err)))
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
}
