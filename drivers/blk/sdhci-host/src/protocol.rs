//! SD/MMC protocol trait adapters and transaction orchestration.

use crate::{
    command::CommandState,
    transfer::{
        map_protocol_error, should_try_dma, submit_read_with_dma_fifo_fallback,
        submit_write_with_dma_fifo_fallback,
    },
    *,
};

enum OwnedSdhciSubmitError {
    Dma {
        error: sdio_host2::Error,
        buffer: dma_api::PreparedDma,
    },
    Cpu {
        error: sdio_host2::Error,
        buffer: dma_api::CpuDmaBuffer,
    },
}

impl OwnedSdhciSubmitError {
    fn into_parts<'a>(self) -> (sdio_host2::Error, sdio_host2::DataBuffer<'a>) {
        match self {
            Self::Dma { error, buffer } => (error, sdio_host2::DataBuffer::Dma(buffer)),
            Self::Cpu { error, buffer } => (error, sdio_host2::DataBuffer::OwnedCpu(buffer)),
        }
    }
}

impl ProtocolSdioHost for Sdhci {
    type Event = Event;
    type DataRequest<'a>
        = DataRequest<'a>
    where
        Self: 'a;
    type BusRequest = ReadyBusRequest;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        self.check_not_poisoned()?;
        Sdhci::submit_command(self, cmd)
    }

    fn poll_command_response(&mut self) -> Result<sdmmc_protocol::CommandResponsePoll, Error> {
        Sdhci::poll_command_response(self)
    }

    fn submit_read_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let buffer = NonNull::new(buf.as_mut_ptr()).ok_or(Error::InvalidArgument)?;
        let mut slot = BlockRequestSlot::default();
        let request = submit_read_with_dma_fifo_fallback(
            self,
            cmd,
            buffer,
            buf.len(),
            block_size,
            block_count,
            &mut slot,
        )?;
        let id = request.id();
        Ok(DataRequest {
            id,
            request: Some(request),
            slot,
            _buffer: PhantomData,
        })
    }

    fn submit_write_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let buffer = NonNull::new(buf.as_ptr() as *mut u8).ok_or(Error::InvalidArgument)?;
        let mut slot = BlockRequestSlot::default();
        let request = submit_write_with_dma_fifo_fallback(
            self,
            cmd,
            buffer,
            buf.len(),
            block_size,
            block_count,
            &mut slot,
        )?;
        let id = request.id();
        Ok(DataRequest {
            id,
            request: Some(request),
            slot,
            _buffer: PhantomData,
        })
    }

    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error> {
        self.service_block_request_response(&mut request.request, request.id, &mut request.slot)
    }

    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        self.apply_bus_width(width)
    }

    fn set_clock(&mut self, _speed: ClockSpeed) -> Result<(), Error> {
        // Clock stabilization has no completion IRQ on generic SDHCI. The
        // compatibility trait cannot carry monotonic time or a wake schedule,
        // so accepting this operation would require a hidden busy-wait.
        Err(Error::UnsupportedCommand)
    }

    fn switch_voltage(&mut self, _voltage: SignalVoltage) -> Result<(), Error> {
        // Voltage switching requires absolute-time settle checks and may need
        // a scheduled platform regulator hook. Native host2 owns that FSM.
        Err(Error::UnsupportedCommand)
    }

    fn execute_tuning(
        &mut self,
        _cmd_index: u8,
        _block_size: core::num::NonZeroU16,
    ) -> Result<(), Error> {
        // The direct compatibility trait cannot express the tuning deadline
        // or IRQ activation. Native host2 owns the bounded tuning FSM.
        Err(Error::UnsupportedCommand)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        if !self.irq.state.source_ready() {
            return Err(Error::InvalidArgument);
        }
        Sdhci::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        Sdhci::disable_completion_irq(self);
        Ok(())
    }

    fn completion_irq_enabled(&self) -> bool {
        Sdhci::completion_irq_enabled(self)
    }

    fn submit_bus_op(&mut self, _op: SdioBusOp) -> Result<Self::BusRequest, Error> {
        // Synchronous reset/clock/tuning helpers cannot satisfy the staged
        // initialization contract. Use `SdioSdmmc::new_host2`.
        Err(Error::UnsupportedCommand)
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        poll_ready_bus_op(request)
    }
}

impl SdioIrqHost for Sdhci {
    type IrqEndpoint = SdhciIrqEndpoint;
    type IrqControl = SdhciIrqControl;

    fn take_irq_source(&mut self) -> Option<SdioIrqSource<Self::IrqEndpoint, Self::IrqControl>> {
        Sdhci::take_irq_source(self)
    }
}

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
                    sdio_host2::DataBuffer::OwnedCpu(_) | sdio_host2::DataBuffer::Dma(_) => {
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
            Some(sdio_host2::DataBuffer::OwnedCpu(_) | sdio_host2::DataBuffer::Dma(_))
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
            unreachable!("owned transaction must contain a data phase")
        };
        if let Err(error) = phase.validate() {
            self.finish_host2_request(host2_id);
            return Err(sdio_host2::SubmitTransactionError::new(
                error,
                sdio_host2::Transaction::with_data(transaction.command, phase),
            ));
        }
        let block_size = u32::from(phase.block_size.get());
        let block_count = phase.block_count.get();
        let direction = match phase.direction {
            sdio_host2::DataDirection::Read => DataDirection::Read,
            sdio_host2::DataDirection::Write => DataDirection::Write,
            _ => unreachable!("validated data phase has a supported direction"),
        };
        let mut slot = BlockRequestSlot::default();
        let submitted = match phase.buffer {
            sdio_host2::DataBuffer::Dma(buffer) => {
                if !should_try_dma(
                    &transaction.command,
                    block_size,
                    block_count,
                    buffer.len().get(),
                    direction,
                ) {
                    Err(OwnedSdhciSubmitError::Dma {
                        error: sdio_host2::Error::Unsupported,
                        buffer,
                    })
                } else if let Some(dma) = self.dma.clone() {
                    let result = match direction {
                        DataDirection::Read => self.submit_prepared_read_blocks(
                            transaction.command.argument,
                            buffer,
                            &dma,
                            &mut slot,
                        ),
                        DataDirection::Write => self.submit_prepared_write_blocks(
                            transaction.command.argument,
                            buffer,
                            &dma,
                            &mut slot,
                        ),
                        DataDirection::None => unreachable!("direction was validated above"),
                        _ => unreachable!("unsupported direction was rejected above"),
                    };
                    result.map_err(|error| OwnedSdhciSubmitError::Dma {
                        error: map_protocol_error(error.error),
                        buffer: error.into_buffer(),
                    })
                } else {
                    Err(OwnedSdhciSubmitError::Dma {
                        error: sdio_host2::Error::Unsupported,
                        buffer,
                    })
                }
            }
            sdio_host2::DataBuffer::OwnedCpu(buffer) => self
                .submit_owned_fifo_data_request(
                    &transaction.command,
                    buffer,
                    block_size,
                    block_count,
                    direction,
                    &mut slot,
                )
                .map_err(|error| OwnedSdhciSubmitError::Cpu {
                    error: map_protocol_error(error.error),
                    buffer: error.into_buffer(),
                }),
            sdio_host2::DataBuffer::Read(_) | sdio_host2::DataBuffer::Write(_) => {
                unreachable!("borrowed buffers were delegated before owned submission")
            }
        };
        match submitted {
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
            Err(error) => {
                self.finish_host2_request(host2_id);
                let (error, buffer) = error.into_parts();
                let data = sdio_host2::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer,
                };
                Err(sdio_host2::SubmitTransactionError::new(
                    error,
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
                    Err(err) => Ok(sdio_host2::RequestPoll::Ready(Err(map_protocol_error(err)))),
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

    fn take_completed_cpu<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CpuDmaBuffer>
    where
        Self: 'a,
    {
        request
            .data
            .as_mut()
            .and_then(|data| data.slot.take_completed_cpu())
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
        let progress = self.poll_host2_bus_state(&mut request.state);
        Ok(self.finish_host2_bus_poll(request, progress))
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
        if request.done {
            return Ok(());
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        let result = self.abort_host2_bus_state(&mut request.state);
        if !matches!(result, Err(sdio_host2::Error::Busy)) {
            if result.is_err() {
                self.poison_dma();
            }
            request.done = true;
            self.finish_host2_request(request.id);
        }
        result
    }

    fn now_ms(&self) -> Option<u64> {
        self.timer.map(HostTimer::now_ms)
    }
}

impl SdioHost2Timed for Sdhci {
    fn poll_transaction_at<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        self.check_host2_transaction_request(request)?;
        if matches!(
            self.command_state,
            CommandState::WaitingInhibit { .. } | CommandState::WaitingWriteGap { .. }
        ) {
            // Broadcom's 32-bit-only register interface requires the block
            // pair and command pair to be separate writes.  At identification
            // clock rates the second write is permitted only after four SD
            // clocks.  Advancing that stage consumes monotonic time, but must
            // not also inspect completion state: only a later IRQ activation
            // is allowed to drive an issued command to completion.
            if self.poll_command_at(now_ns).is_ok() {
                return Ok(sdio_host2::RequestPoll::Pending);
            }
        }
        <Self as sdio_host2::SdioHost>::poll_transaction(self, request)
    }

    fn transaction_wake_at<'a>(&self, request: &Self::TransactionRequest<'a>) -> Option<u64>
    where
        Self: 'a,
    {
        if request.done
            || request.owner != self.host2_owner()
            || self.host2_active_id != Some(request.id)
        {
            return None;
        }
        self.command_program_wake_at()
    }

    fn poll_bus_op_at(
        &mut self,
        request: &mut Self::BusRequest,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError> {
        self.check_host2_bus_request(request)?;
        let progress = self.poll_host2_bus_state_at(&mut request.state, now_ns);
        Ok(self.finish_host2_bus_poll(request, progress))
    }

    fn bus_op_wake_at(&self, request: &Self::BusRequest) -> Option<u64> {
        match &request.state {
            BusRequestState::Reset {
                state: SdhciResetState::WaitHook { wake_at_ns },
                ..
            } => Some(*wake_at_ns),
            BusRequestState::Reset {
                state: SdhciResetState::WaitController { wait },
                ..
            }
            | BusRequestState::SetClock(
                SdhciClockState::ExternalEnable { wait, .. }
                | SdhciClockState::InternalWaitStable { wait, .. },
            )
            | BusRequestState::ExecuteTuning(SdhciTuningState::Wait { wait, .. }) => {
                Some(wait.wake_at_ns)
            }
            BusRequestState::SetSignalVoltage(SdhciVoltageState::WaitVsw {
                wake_at_ns, ..
            }) => Some(*wake_at_ns),
            _ => None,
        }
    }
}
