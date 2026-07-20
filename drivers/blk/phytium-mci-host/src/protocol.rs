use super::*;

impl ProtocolSdioHost for PhytiumMci {
    type Event = Event;
    type DataRequest<'a> = DataRequest<'a>;
    type BusRequest = ReadyBusRequest;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        self.check_not_poisoned()?;
        PhytiumMci::submit_command(self, cmd)
    }

    fn poll_command_response(&mut self) -> Result<sdmmc_protocol::CommandResponsePoll, Error> {
        PhytiumMci::poll_command_response(self)
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
        self.set_bus_width(width);
        Ok(())
    }

    fn set_clock(&mut self, _speed: ClockSpeed) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    fn switch_voltage(&mut self, _voltage: SignalVoltage) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        if !self.irq.state.source_ready() {
            return Err(Error::InvalidArgument);
        }
        PhytiumMci::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        PhytiumMci::disable_completion_irq(self);
        Ok(())
    }

    fn completion_irq_enabled(&self) -> bool {
        PhytiumMci::completion_irq_enabled(self)
    }

    fn submit_bus_op(&mut self, _op: SdioBusOp) -> Result<Self::BusRequest, Error> {
        Err(Error::UnsupportedCommand)
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        poll_ready_bus_op(request)
    }
}

impl SdioIrqHost for PhytiumMci {
    type IrqEndpoint = PhytiumMciIrqEndpoint;
    type IrqControl = PhytiumMciIrqControl;

    fn take_irq_source(&mut self) -> Option<SdioIrqSource<Self::IrqEndpoint, Self::IrqControl>> {
        PhytiumMci::take_irq_source(self)
    }
}

fn submit_read_with_dma_fifo_fallback(
    host: &mut PhytiumMci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if should_try_dma(cmd, block_size, block_count, len, DataDirection::Read)
        && let Some(dma) = host.dma.clone()
    {
        match host.submit_read_blocks(
            cmd.argument,
            buffer,
            NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
            Some(&dma),
            BlockTransferMode::Dma,
            slot,
        ) {
            Ok(request) => return Ok(request),
            Err(err) if can_fallback_to_fifo(err) => {}
            Err(err) => return Err(err),
        }
    }

    host.submit_fifo_data_request(
        cmd,
        buffer,
        len,
        block_size,
        block_count,
        DataDirection::Read,
        slot,
    )
}

fn submit_write_with_dma_fifo_fallback(
    host: &mut PhytiumMci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if should_try_dma(cmd, block_size, block_count, len, DataDirection::Write)
        && let Some(dma) = host.dma.clone()
    {
        match host.submit_write_blocks(
            cmd.argument,
            buffer,
            NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
            Some(&dma),
            BlockTransferMode::Dma,
            slot,
        ) {
            Ok(request) => return Ok(request),
            Err(err) if can_fallback_to_fifo(err) => {}
            Err(err) => return Err(err),
        }
    }

    host.submit_fifo_data_request(
        cmd,
        buffer,
        len,
        block_size,
        block_count,
        DataDirection::Write,
        slot,
    )
}

pub(crate) fn should_try_dma(
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

fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

impl PhytiumMci {
    pub fn block_buffer_config(&self, mode: BlockTransferMode) -> BlockBufferConfig {
        match mode {
            BlockTransferMode::Fifo => {
                BlockBufferConfig::new(NonZeroUsize::new(512).unwrap(), 1, None)
            }
            BlockTransferMode::Dma => {
                BlockBufferConfig::new(NonZeroUsize::new(512).unwrap(), 512, Some(self.dma_mask))
            }
            // Future BlockTransferMode variants fall back to the conservative Fifo config.
            _ => BlockBufferConfig::new(NonZeroUsize::new(512).unwrap(), 1, None),
        }
    }
}
