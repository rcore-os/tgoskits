//! Phytium MCI/FSDIF host controller backend for `sdmmc-protocol`.
//!
//! The register layout is the Phytium Memory Card Interface found on E2000
//! class SoCs. It is close to the DesignWare MSHC programming model, with
//! Phytium-specific clock-source and timing registers.
//!
//! # Scope
//!
//! - **Implemented**: controller/FIFO reset, power and clock setup, Phytium
//!   timing tables, 1-bit / 4-bit / 8-bit bus selection, command response
//!   decoding, FIFO and IDMAC block transfers, and stable IRQ event extraction.
//! - **Out of scope for this crate**: FDT/ACPI probe, MMIO remapping, IRQ
//!   registration, pad-controller programming, OS sleeps/wakeups, and rdif-block
//!   registration.
//! - **Implemented for block I/O**: IDMAC descriptor setup, DMA buffer mapping,
//!   DMA block read/write polling, and FIFO fallback in the native protocol
//!   data path.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::sync::Arc;
use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

mod command;
mod dma;
mod host;
pub mod rdif;
mod regs;
mod timing;

pub use dma::{BlockRequest, BlockRequestSlot, RequestId};
use host::uhs_bits_after_voltage;
pub use host::{DEFAULT_FIFO_OFFSET, PhytiumMci};
use regs::RegisterBlockVolatileFieldAccess;
pub use sdmmc_protocol::block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState,
};
use sdmmc_protocol::{
    DataCommandPoll, OperationPoll,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, ReadyBusRequest,
        SdioBusOp, SdioHost as ProtocolSdioHost, SdioIrqHandle, SdioIrqHost, SignalVoltage,
        poll_ready_bus_op, submit_ready_bus_op,
    },
};

/// Stable controller event extracted from Phytium MCI raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// A command response has completed.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// Receive FIFO can be drained.
    ReceiveReady,
    /// Transmit FIFO can accept more data.
    TransmitReady,
    /// One or more controller error bits are pending.
    Error { raw_status: u32 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { raw_status: u32 },
}

impl HostEvent for Event {
    fn kind(&self) -> HostEventKind {
        match self {
            Event::None => HostEventKind::None,
            Event::CommandComplete => HostEventKind::CommandComplete,
            Event::TransferComplete => HostEventKind::TransferComplete,
            Event::ReceiveReady => HostEventKind::ReceiveReady,
            Event::TransmitReady => HostEventKind::TransmitReady,
            Event::Error { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
        }
    }

    fn source(&self) -> HostEventSource {
        match self {
            Event::CommandComplete => HostEventSource::Command,
            Event::TransferComplete | Event::ReceiveReady | Event::TransmitReady => {
                HostEventSource::Data
            }
            Event::None | Event::Error { .. } | Event::Other { .. } => HostEventSource::Controller,
        }
    }
}

pub(crate) const MCI_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const MCI_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const MCI_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const MCI_INT_TXDR: u32 = 1 << 4;
pub(crate) const MCI_INT_RXDR: u32 = 1 << 5;
pub(crate) const MCI_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const MCI_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const MCI_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const MCI_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const MCI_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const MCI_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const MCI_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
pub(crate) const MCI_INT_START_BIT_ERROR: u32 = 1 << 13;
pub(crate) const MCI_INT_END_BIT_ERROR: u32 = 1 << 15;
pub(crate) const MCI_INT_ERROR_MASK: u32 = MCI_INT_RESPONSE_ERROR
    | MCI_INT_RESPONSE_CRC_ERROR
    | MCI_INT_DATA_CRC_ERROR
    | MCI_INT_RESPONSE_TIMEOUT
    | MCI_INT_DATA_READ_TIMEOUT
    | MCI_INT_HOST_TIMEOUT
    | MCI_INT_FIFO_UNDER_OVER_RUN
    | MCI_INT_HARDWARE_LOCKED_WRITE
    | MCI_INT_START_BIT_ERROR
    | MCI_INT_END_BIT_ERROR;

pub(crate) const MCI_IDSTS_TRANSMIT: u32 = 1 << 0;
pub(crate) const MCI_IDSTS_RECEIVE: u32 = 1 << 1;
pub(crate) const MCI_IDSTS_FATAL_BUS_ERROR: u32 = 1 << 2;
pub(crate) const MCI_IDSTS_DESCRIPTOR_UNAVAILABLE: u32 = (1 << 3) | (1 << 4);
pub(crate) const MCI_IDSTS_CARD_ERROR_SUMMARY: u32 = 1 << 5;
pub(crate) const MCI_IDSTS_ERROR_MASK: u32 =
    MCI_IDSTS_FATAL_BUS_ERROR | MCI_IDSTS_DESCRIPTOR_UNAVAILABLE | MCI_IDSTS_CARD_ERROR_SUMMARY;

pub struct DataRequest<'a> {
    id: RequestId,
    request: Option<BlockRequest>,
    slot: BlockRequestSlot,
    _buffer: PhantomData<&'a [u8]>,
}

pub struct TransactionRequest<'a> {
    owner: usize,
    id: u64,
    done: bool,
    kind: TransactionRequestKind,
    data: Option<DataRequest<'a>>,
}

enum TransactionRequestKind {
    Command { response: sdio_host2::ResponseType },
    Data { response: sdio_host2::ResponseType },
}

impl<'a> TransactionRequest<'a> {
    fn command(owner: usize, id: u64, response: sdio_host2::ResponseType) -> Self {
        Self {
            owner,
            id,
            done: false,
            kind: TransactionRequestKind::Command { response },
            data: None,
        }
    }

    fn data(
        owner: usize,
        id: u64,
        request: DataRequest<'a>,
        response: sdio_host2::ResponseType,
    ) -> Self {
        Self {
            owner,
            id,
            done: false,
            kind: TransactionRequestKind::Data { response },
            data: Some(request),
        }
    }
}

pub struct BusRequest {
    owner: usize,
    id: u64,
    done: bool,
    state: BusRequestState,
}

impl BusRequest {
    fn pending(owner: usize, id: u64, state: BusRequestState) -> Self {
        Self {
            owner,
            id,
            done: false,
            state,
        }
    }
}

enum BusRequestState {
    ResetAll(PhytiumResetState),
    ResetDataLine { started: bool, polls: u32 },
    PowerOn,
    PowerOff,
    SetClock(PhytiumClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(PhytiumVoltageState),
}

enum PhytiumResetState {
    Start,
    WaitReset { polls: u32 },
    InitClock(PhytiumClockState),
}

enum PhytiumClockState {
    Start {
        timing: timing::TimingTable,
    },
    WaitExternalClock {
        polls: u32,
        timing: timing::TimingTable,
    },
    WaitDisable {
        polls: u32,
        timing: timing::TimingTable,
    },
    ProgramDivider {
        timing: timing::TimingTable,
    },
    WaitEnable {
        polls: u32,
    },
}

enum PhytiumVoltageState {
    Start(SignalVoltage),
    WaitUpdate { polls: u32 },
}

const PHYTIUM_RESET_POLLS: u32 = 1_000_000;
const PHYTIUM_CLOCK_POLLS: u32 = 1_000_000;

/// Cloneable, sync-safe Phytium MCI IRQ top-half handle.
#[derive(Clone)]
pub struct PhytiumMciIrqHandle {
    pub(crate) irq: Arc<host::IrqCore>,
}

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
        self.poll_block_request_response(&mut request.request, request.id, &mut request.slot)
    }

    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        self.set_bus_width(width);
        Ok(())
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        self.program_timing(timing::TimingTable::sd_for_speed(speed)?)
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        self.set_signal_voltage(voltage)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        PhytiumMci::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        PhytiumMci::disable_completion_irq(self);
        Ok(())
    }

    fn handle_irq(&mut self) -> Self::Event {
        self.irq_handle().handle_irq()
    }

    fn submit_bus_op(&mut self, op: SdioBusOp) -> Result<Self::BusRequest, Error> {
        submit_ready_bus_op(self, op)
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        poll_ready_bus_op(request)
    }
}

impl SdioIrqHost for PhytiumMci {
    type IrqHandle = PhytiumMciIrqHandle;

    fn irq_handle(&self) -> Self::IrqHandle {
        PhytiumMci::irq_handle(self)
    }

    fn completion_irq_enabled(&self) -> bool {
        PhytiumMci::completion_irq_enabled(self)
    }
}

impl sdio_host2::SdioHost for PhytiumMci {
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
        if !should_try_dma(
            &transaction.command,
            block_size,
            block_count,
            buffer.len().get(),
            match phase.direction {
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
            },
        ) {
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
        let submit = match phase.direction {
            sdio_host2::DataDirection::Read => self.submit_prepared_read_blocks(
                transaction.command.argument,
                buffer,
                &dma,
                &mut slot,
            ),
            sdio_host2::DataDirection::Write => self.submit_prepared_write_blocks(
                transaction.command.argument,
                buffer,
                &dma,
                &mut slot,
            ),
            _ => unreachable!("unsupported direction returned before submit"),
        };
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
}

impl PhytiumMci {
    fn physical_bus_idle(&self) -> bool {
        matches!(self.command_state, command::CommandState::Idle)
            && self.pending_data.is_none()
            && self.data_blocks_remaining == 0
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

    fn prepare_host2_bus_op(
        &self,
        op: sdio_host2::BusOp,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        match op {
            sdio_host2::BusOp::ResetAll => Ok(BusRequestState::ResetAll(PhytiumResetState::Start)),
            sdio_host2::BusOp::ResetCommandLine => Err(sdio_host2::Error::Unsupported),
            sdio_host2::BusOp::ResetDataLine => Ok(BusRequestState::ResetDataLine {
                started: false,
                polls: 0,
            }),
            sdio_host2::BusOp::PowerOn => Ok(BusRequestState::PowerOn),
            sdio_host2::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdio_host2::BusOp::SetClock(speed) => {
                let timing =
                    timing::TimingTable::sd_for_speed(speed).map_err(map_protocol_error)?;
                Ok(BusRequestState::SetClock(PhytiumClockState::Start {
                    timing,
                }))
            }
            sdio_host2::BusOp::SetClockHz(_) => Err(sdio_host2::Error::Unsupported),
            sdio_host2::BusOp::SetBusWidth(width) => match width {
                BusWidth::Bit1 | BusWidth::Bit4 | BusWidth::Bit8 => {
                    Ok(BusRequestState::SetBusWidth(width))
                }
                _ => Err(sdio_host2::Error::Unsupported),
            },
            sdio_host2::BusOp::SetSignalVoltage(voltage) => {
                uhs_bits_after_voltage(self.regs.uhs().read(), voltage)
                    .map_err(map_protocol_error)?;
                Ok(BusRequestState::SetSignalVoltage(
                    PhytiumVoltageState::Start(voltage),
                ))
            }
            sdio_host2::BusOp::ExecuteTuning { .. } => Err(sdio_host2::Error::Unsupported),
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    fn poll_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            BusRequestState::ResetAll(reset) => self.poll_host2_reset_all(reset),
            BusRequestState::ResetDataLine { started, polls } => {
                self.poll_host2_fifo_reset(started, polls)
            }
            BusRequestState::PowerOn => {
                self.regs.pwren().write(1);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::PowerOff => {
                self.regs.pwren().write(0);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.poll_host2_clock(clock),
            BusRequestState::SetBusWidth(width) => {
                PhytiumMci::set_bus_width(self, *width);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => self.poll_host2_voltage(voltage),
        }
    }

    fn poll_host2_reset_all(
        &mut self,
        state: &mut PhytiumResetState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            PhytiumResetState::Start => {
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.ctrl().update(|r| {
                    r.with_use_internal_dmac(false)
                        .with_dma_enable(false)
                        .with_int_enable(false)
                });
                self.regs.ctrl().update(|r| {
                    r.with_controller_reset(true)
                        .with_fifo_reset(true)
                        .with_dma_reset(true)
                });
                *state = PhytiumResetState::WaitReset { polls: 0 };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumResetState::WaitReset { polls } => {
                let ctrl = self.regs.ctrl().read();
                if !ctrl.controller_reset() && !ctrl.fifo_reset() && !ctrl.dma_reset() {
                    self.regs.intmask().write(0);
                    self.regs.idinten().write(0);
                    self.clear_all_int_status();
                    self.regs.idsts().write(u32::MAX);
                    self.irq.state.clear_all();
                    self.clear_completion_irq_enabled();
                    self.regs.ctype().write(crate::regs::CType::new());
                    self.regs.uhs().write(crate::regs::Uhs::new());
                    self.regs.tmout().write(0xffff_ffff);
                    self.regs.pwren().write(1);
                    self.regs.fifoth().write(crate::host::FIFO_THRESHOLD);
                    self.write_ext_reg(
                        crate::regs::CARD_THRCTL_OFFSET,
                        crate::host::CARD_READ_THRESHOLD_ENABLE
                            | crate::host::CARD_READ_THRESHOLD_DEPTH8,
                    );
                    *state = PhytiumResetState::InitClock(PhytiumClockState::Start {
                        timing: timing::TimingTable::sd_for_speed(ClockSpeed::Identification)
                            .map_err(map_protocol_error)?,
                    });
                    return Ok(sdio_host2::RequestPoll::Pending);
                }
                if *polls >= PHYTIUM_RESET_POLLS {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumResetState::InitClock(clock) => self.poll_host2_clock(clock),
        }
    }

    fn poll_host2_fifo_reset(
        &mut self,
        started: &mut bool,
        polls: &mut u32,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        if !*started {
            self.regs.ctrl().update(|r| r.with_fifo_reset(true));
            *started = true;
        }
        if !self.regs.ctrl().read().fifo_reset() {
            return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
        }
        if *polls >= PHYTIUM_RESET_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::DataRead,
            ))));
        }
        *polls += 1;
        Ok(sdio_host2::RequestPoll::Pending)
    }

    fn poll_host2_clock(
        &mut self,
        state: &mut PhytiumClockState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            PhytiumClockState::Start { timing } => {
                self.use_hold_reg = timing.use_hold;
                self.write_ext_reg(crate::regs::CLK_SRC_OFFSET, 0);
                self.write_ext_reg(crate::regs::CLK_SRC_OFFSET, timing.clk_src);
                *state = PhytiumClockState::WaitExternalClock {
                    polls: 0,
                    timing: *timing,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::WaitExternalClock { polls, timing } => {
                if self.regs.cksts().read().ready() {
                    self.regs.clkena().write(crate::regs::ClkEna::new());
                    self.start_update_clock(false);
                    *state = PhytiumClockState::WaitDisable {
                        polls: 0,
                        timing: *timing,
                    };
                    return Ok(sdio_host2::RequestPoll::Pending);
                }
                if *polls >= PHYTIUM_CLOCK_POLLS {
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::WaitDisable { polls, timing } => {
                if self.poll_update_clock_complete(polls)? {
                    *state = PhytiumClockState::ProgramDivider { timing: *timing };
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::ProgramDivider { timing } => {
                self.regs.clkdiv().write(timing.clk_div);
                self.regs
                    .clkena()
                    .write(crate::regs::ClkEna::new().with_cclk_enable(1));
                self.start_update_clock(false);
                *state = PhytiumClockState::WaitEnable { polls: 0 };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumClockState::WaitEnable { polls } => {
                if self.poll_update_clock_complete(polls)? {
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    fn poll_host2_voltage(
        &mut self,
        state: &mut PhytiumVoltageState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            PhytiumVoltageState::Start(voltage) => {
                let next = uhs_bits_after_voltage(self.regs.uhs().read(), *voltage)
                    .map_err(map_protocol_error)?;
                self.regs.uhs().write(next);
                self.start_update_clock(matches!(*voltage, SignalVoltage::V180));
                *state = PhytiumVoltageState::WaitUpdate { polls: 0 };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            PhytiumVoltageState::WaitUpdate { polls } => {
                if self.poll_update_clock_complete(polls)? {
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    fn start_update_clock(&self, voltage_switch: bool) {
        self.regs.cmd().write(
            crate::regs::Cmd::new()
                .with_start_cmd(true)
                .with_wait_prvdata_complete(true)
                .with_update_clock_registers_only(true)
                .with_volt_switch(voltage_switch),
        );
    }

    fn poll_update_clock_complete(&self, polls: &mut u32) -> Result<bool, sdio_host2::Error> {
        if !self.regs.cmd().read().start_cmd() {
            return Ok(true);
        }
        if *polls >= PHYTIUM_CLOCK_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        *polls += 1;
        Ok(false)
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

    fn abort_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<(), sdio_host2::Error> {
        match state {
            BusRequestState::ResetAll(_)
            | BusRequestState::SetClock(_)
            | BusRequestState::SetSignalVoltage(_) => {
                self.reset_and_init_preserving_irq()
                    .map_err(map_protocol_error)?;
            }
            BusRequestState::ResetDataLine { started, .. } if *started => {
                self.reset_fifo(sdmmc_protocol::Phase::DataRead)
                    .map_err(map_protocol_error)?;
            }
            BusRequestState::PowerOn
            | BusRequestState::PowerOff
            | BusRequestState::SetBusWidth(_) => {}
            BusRequestState::ResetDataLine { .. } => {}
        }
        self.pending_data = None;
        self.data_blocks_remaining = 0;
        self.command_state = command::CommandState::Idle;
        Ok(())
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

fn should_try_dma(
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

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU32};

    use sdmmc_protocol::{
        BlockTransferMode,
        cmd::CMD0,
        response::ResponseType,
        sdio::{ClockSpeed, SignalVoltage},
    };

    use crate::{
        PhytiumMci,
        command::encode_command,
        regs::{Ctrl, Uhs},
        timing::{MediaKind, TimingTable},
    };

    #[test]
    fn sd_timing_table_matches_phytium_sd_values() {
        let init = TimingTable::for_speed(ClockSpeed::Identification, MediaKind::Sd).unwrap();
        assert_eq!(init.clk_div, 0x7e7dfa);
        assert_eq!(init.clk_src, 0x000502);
        assert!(init.use_hold);

        let hs = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Sd).unwrap();
        assert_eq!(hs.clk_div, 0x030204);
        assert_eq!(hs.clk_src, 0x000502);
        assert!(hs.use_hold);
    }

    #[test]
    fn mmc_timing_table_uses_mmc_specific_rates() {
        let default = TimingTable::for_speed(ClockSpeed::Default, MediaKind::Mmc).unwrap();
        assert_eq!(default.target_hz, 26_000_000);

        let high = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Mmc).unwrap();
        assert_eq!(high.target_hz, 52_000_000);
    }

    #[test]
    fn unsupported_sd_clock_modes_are_rejected() {
        assert!(TimingTable::sd_for_speed(ClockSpeed::Sdr104).is_err());
    }

    #[test]
    fn ctrl_register_bits_match_phytium_mci_layout() {
        let reg = Ctrl::new()
            .with_int_enable(true)
            .with_dma_enable(true)
            .with_read_wait(true)
            .with_use_internal_dmac(true);

        assert_eq!(reg.into_bits(), (1 << 4) | (1 << 5) | (1 << 6) | (1 << 25));
    }

    #[test]
    fn r3_command_encoding_does_not_enable_crc_check() {
        let cmd = sdmmc_protocol::cmd::Command::new(1, 0, ResponseType::R3);
        let reg = encode_command(&cmd, None);
        assert!(reg.response_expect());
        assert!(!reg.check_response_crc());
    }

    #[test]
    fn cmd0_encoding_sends_initialization_clocks() {
        let reg = encode_command(&CMD0, None);
        assert!(reg.send_initialization());
        assert!(!reg.response_expect());
    }

    #[test]
    fn cmd12_encoding_marks_stop_abort() {
        let reg = encode_command(&sdmmc_protocol::cmd::CMD12, None);
        assert!(reg.stop_abort_cmd());
    }

    #[test]
    fn uhs_voltage_bit_tracks_signal_voltage() {
        let v180 = crate::host::uhs_bits_after_voltage(Uhs::new(), SignalVoltage::V180).unwrap();
        assert_eq!(v180.volt(), 1);

        let v330 = crate::host::uhs_bits_after_voltage(v180, SignalVoltage::V330).unwrap();
        assert_eq!(v330.volt(), 0);
    }

    #[test]
    fn command_register_keeps_hold_register_optional() {
        let cmd = sdmmc_protocol::cmd::Command::new(17, 0, ResponseType::R1);
        let without_hold = encode_command(&cmd, None).with_use_hold_reg(false);
        assert!(!without_hold.use_hold_reg());
        assert_eq!(without_hold.cmd_index(), 17);
    }

    #[test]
    fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
        let mut host = unsafe { PhytiumMci::new_from_addr(0x1000_0000) };
        host.command_state = crate::command::CommandState::Issued {
            cmd: sdmmc_protocol::cmd::Command::new(0, 0, ResponseType::None),
            polls: 0,
        };
        let mut buf = [0u8; 512];
        let data = sdio_host2::DataPhase::read(
            NonZeroU16::new(512).unwrap(),
            NonZeroU32::new(1).unwrap(),
            &mut buf,
        )
        .unwrap();
        let tx = sdio_host2::Transaction::with_data(
            sdmmc_protocol::cmd::Command::new(17, 0, ResponseType::R1),
            data,
        );

        let err = match unsafe {
            <PhytiumMci as sdio_host2::SdioHost>::submit_transaction(&mut host, tx)
        } {
            Ok(_) => panic!("busy host accepted a second transaction"),
            Err(err) => err,
        };

        assert_eq!(err, sdio_host2::Error::Busy);
        assert!(host.pending_data.is_none());
        assert_eq!(host.data_blocks_remaining, 0);
    }

    #[test]
    fn exposes_block_buffer_constraints() {
        let host = unsafe { PhytiumMci::new_from_addr(0x1000_0000) };

        let fifo = host.block_buffer_config(BlockTransferMode::Fifo);
        assert_eq!(fifo.block_size.get(), 512);
        assert_eq!(fifo.align, 1);
        assert_eq!(fifo.dma_mask, None);

        let dma = host.block_buffer_config(BlockTransferMode::Dma);
        assert_eq!(dma.block_size.get(), 512);
        assert_eq!(dma.align, 512);
        assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
    }
}
