use super::{
    request::{
        TransactionRequestKind, submit_borrowed_read_data, submit_borrowed_write_data,
        supports_owned_dma_transaction,
    },
    *,
};

impl sdmmc_host::SdMmcHost for PhytiumMci {
    type TransactionRequest<'a>
        = TransactionRequest<'a>
    where
        Self: 'a;
    type BusRequest = BusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: sdmmc_host::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdmmc_host::Error>
    where
        Self: 'a,
    {
        self.check_not_poisoned().map_err(map_protocol_error)?;
        if !self.physical_bus_idle() {
            return Err(sdmmc_host::Error::Busy);
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
                    sdmmc_host::DataBuffer::Read(buf) => {
                        if !matches!(phase.direction, sdmmc_host::DataDirection::Read) {
                            self.finish_host2_request(id);
                            return Err(sdmmc_host::Error::InvalidArgument);
                        }
                        submit_borrowed_read_data(
                            self,
                            &transaction.command,
                            buf,
                            block_size,
                            block_count,
                        )
                    }
                    sdmmc_host::DataBuffer::Write(buf) => {
                        if !matches!(phase.direction, sdmmc_host::DataDirection::Write) {
                            self.finish_host2_request(id);
                            return Err(sdmmc_host::Error::InvalidArgument);
                        }
                        submit_borrowed_write_data(
                            self,
                            &transaction.command,
                            buf,
                            block_size,
                            block_count,
                        )
                    }
                    sdmmc_host::DataBuffer::Dma(_) => {
                        self.finish_host2_request(id);
                        return Err(sdmmc_host::Error::InvalidArgument);
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
        transaction: sdmmc_host::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdmmc_host::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        if let Err(err) = self.check_not_poisoned() {
            return Err(sdmmc_host::SubmitTransactionError::new(
                map_protocol_error(err),
                transaction,
            ));
        }
        if !matches!(
            transaction.data.as_ref().map(|data| &data.buffer),
            Some(sdmmc_host::DataBuffer::Dma(_))
        ) {
            return Err(sdmmc_host::SubmitTransactionError::new(
                sdmmc_host::Error::Unsupported,
                transaction,
            ));
        }
        if !self.physical_bus_idle() {
            return Err(sdmmc_host::SubmitTransactionError::new(
                sdmmc_host::Error::Busy,
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
        let sdmmc_host::DataBuffer::Dma(buffer) = phase.buffer else {
            unreachable!("checked for DMA data buffer above")
        };
        let direction = match phase.direction {
            sdmmc_host::DataDirection::Read => DataDirection::Read,
            sdmmc_host::DataDirection::Write => DataDirection::Write,
            _ => {
                self.finish_host2_request(host2_id);
                let data = sdmmc_host::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer: sdmmc_host::DataBuffer::Dma(buffer),
                };
                return Err(sdmmc_host::SubmitTransactionError::new(
                    sdmmc_host::Error::Unsupported,
                    sdmmc_host::Transaction::with_data(transaction.command, data),
                ));
            }
        };
        if !supports_owned_dma_transaction(block_size, block_count, buffer.len().get(), direction) {
            self.finish_host2_request(host2_id);
            let data = sdmmc_host::DataPhase {
                direction: phase.direction,
                block_size: phase.block_size,
                block_count: phase.block_count,
                buffer: sdmmc_host::DataBuffer::Dma(buffer),
            };
            return Err(sdmmc_host::SubmitTransactionError::new(
                sdmmc_host::Error::Unsupported,
                sdmmc_host::Transaction::with_data(transaction.command, data),
            ));
        }
        let Some(dma) = self.dma.take() else {
            self.finish_host2_request(host2_id);
            let data = sdmmc_host::DataPhase {
                direction: phase.direction,
                block_size: phase.block_size,
                block_count: phase.block_count,
                buffer: sdmmc_host::DataBuffer::Dma(buffer),
            };
            return Err(sdmmc_host::SubmitTransactionError::new(
                sdmmc_host::Error::Unsupported,
                sdmmc_host::Transaction::with_data(transaction.command, data),
            ));
        };
        let mut slot = BlockRequestSlot::default();
        let submit = self.submit_prepared_data_command(
            dma::PreparedDataCommand::new(transaction.command, block_size, block_count, direction),
            buffer,
            &dma,
            &mut slot,
        );
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
                let data = sdmmc_host::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer: sdmmc_host::DataBuffer::Dma(buffer),
                };
                Err(sdmmc_host::SubmitTransactionError::new(
                    map_protocol_error(error),
                    sdmmc_host::Transaction::with_data(transaction.command, data),
                ))
            }
        }
    }

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: sdmmc_host::ProgressCause,
    ) -> Result<sdmmc_host::RequestProgress<sdmmc_host::RawResponse>, sdmmc_host::AdvanceRequestError>
    where
        Self: 'a,
    {
        self.check_host2_transaction_request(request)?;
        let acknowledged_irq = cause == sdmmc_host::ProgressCause::AcknowledgedIrq;
        request.acknowledged_irq |= acknowledged_irq;
        if !acknowledged_irq && !self.command_needs_register_retry() {
            return Ok(sdmmc_host::RequestProgress::WaitingForIrq);
        }
        // START_CMD may still be set when the hard IRQ acknowledges a fast
        // command. Preserve that acknowledgement across the register-only
        // retry so the maintenance thread can consume the already-latched
        // completion after the controller accepts the command. This does not
        // turn register retries into completion polling: only a request that
        // previously received an acknowledged IRQ may take this path.
        let progress_cause = if request.acknowledged_irq && self.command_needs_register_retry() {
            sdmmc_host::ProgressCause::AcknowledgedIrq
        } else {
            cause
        };
        match request.kind {
            TransactionRequestKind::Command { response } => {
                match self.advance_command_response(progress_cause) {
                    Ok(CommandResponseProgress::Pending) => Ok(self.pending_transaction_progress()),
                    Ok(CommandResponseProgress::Complete(resp)) if request.acknowledged_irq => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdmmc_host::RequestProgress::Complete(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(CommandResponseProgress::Complete(_)) => {
                        Ok(sdmmc_host::RequestProgress::WaitingForIrq)
                    }
                    Err(err) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdmmc_host::RequestProgress::Complete(Err(
                            map_protocol_error(err),
                        )))
                    }
                }
            }
            TransactionRequestKind::Data { response } => {
                let Some(data) = request.data.as_mut() else {
                    let recovery = self.abort_host2_transaction_request(request).err();
                    return Ok(sdmmc_host::RequestProgress::Complete(Err(
                        recovery.unwrap_or(sdmmc_host::Error::InvalidArgument)
                    )));
                };
                match self.advance_block_request_response(
                    &mut data.request,
                    data.id,
                    &mut data.slot,
                    progress_cause,
                ) {
                    Ok(DataCommandProgress::Pending) => Ok(self.pending_transaction_progress()),
                    Ok(DataCommandProgress::Complete(resp)) if request.acknowledged_irq => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdmmc_host::RequestProgress::Complete(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(DataCommandProgress::Complete(_)) => {
                        Ok(sdmmc_host::RequestProgress::WaitingForIrq)
                    }
                    Err(err) => {
                        let _ = self.abort_host2_transaction_request(request);
                        Ok(sdmmc_host::RequestProgress::Complete(Err(
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
    ) -> Result<(), sdmmc_host::Error>
    where
        Self: 'a,
    {
        if request.done {
            return Ok(());
        }
        if request.owner != self.host2_owner() {
            return Err(sdmmc_host::Error::InvalidArgument);
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
        op: sdmmc_host::BusOp,
    ) -> Result<Self::BusRequest, sdmmc_host::Error> {
        self.check_not_poisoned().map_err(map_protocol_error)?;
        if !self.physical_bus_idle() {
            return Err(sdmmc_host::Error::Busy);
        }
        let state = self.prepare_host2_bus_op(op)?;
        let owner = self.host2_owner();
        let id = self.start_host2_request();
        Ok(BusRequest::pending(owner, id, state))
    }

    fn advance_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
        cause: sdmmc_host::ProgressCause,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::AdvanceRequestError> {
        self.check_host2_bus_request(request)?;
        if cause == sdmmc_host::ProgressCause::AcknowledgedIrq {
            return Ok(sdmmc_host::RequestProgress::RegisterPending {
                retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
            });
        }
        match self.advance_host2_bus_state(&mut request.state) {
            Ok(sdmmc_host::RequestProgress::RegisterPending { retry_after }) => {
                Ok(sdmmc_host::RequestProgress::RegisterPending { retry_after })
            }
            Ok(sdmmc_host::RequestProgress::WaitingForIrq) => {
                Ok(sdmmc_host::RequestProgress::RegisterPending {
                    retry_after: PHYTIUM_REGISTER_RETRY_DELAY,
                })
            }
            Ok(sdmmc_host::RequestProgress::Complete(Ok(()))) => {
                self.complete_host2_bus_request(request);
                Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
            }
            Ok(sdmmc_host::RequestProgress::Complete(Err(err))) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdmmc_host::RequestProgress::Complete(Err(err)))
            }
            Err(err) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdmmc_host::RequestProgress::Complete(Err(err)))
            }
        }
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdmmc_host::Error> {
        if request.done {
            return Ok(());
        }
        if request.owner != self.host2_owner() {
            return Err(sdmmc_host::Error::InvalidArgument);
        }
        let result = self.abort_host2_bus_state(&mut request.state);
        request.done = true;
        self.finish_host2_request(request.id);
        result
    }
}
