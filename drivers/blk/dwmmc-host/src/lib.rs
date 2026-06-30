//! Synopsys DesignWare Mobile Storage Host Controller (DW_mshc) backend
//! for the [`sdmmc-protocol`](sdmmc_protocol) driver crate.
//!
//! Implements [`sdio_host2::SdioHost`] for the IP block known
//! variously as DWC_mobile_storage, dw_mshc, dw_mmc (Linux), or simply
//! the "Synopsys SD/MMC controller" — the same core used in Rockchip
//! RK33xx/RK35xx, Allwinner A-series, StarFive JH7110, and a long
//! tail of mid-range SoCs. Block I/O can be submitted through either the
//! FIFO path or the internal DMAC (IDMAC) path with the same poll contract.
//!
//! # Scope
//!
//! - **Implemented**: PIO data transfer over the 0x100/0x200/0x400
//!   FIFO (configurable), IDMAC descriptor transfers,
//!   1-bit / 4-bit / 8-bit bus selection,
//!   default / high-speed / UHS-I / HS200 clocking, DW_mshc UHS DDR
//!   and 1.8 V signaling bits, R1/R1b/R2/R3/R4/R5/R6/R7 response
//!   decoding, software reset.
//! - **Out of scope (for now)**: external-DMA path, controller-specific
//!   DLL/strobe/tuning window setup (CMD19/CMD21).
//!
//! # Usage
//!
//! ```rust,no_run
//! use core::ptr::NonNull;
//!
//! use dwmmc_host::DwMmc;
//! use sdmmc_protocol::sdio::{SdioInitScratch, SdioSdmmc};
//!
//! // SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file
//! // the caller has exclusive access to.
//! let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
//! let mut host = unsafe { DwMmc::new(mmio) };
//! host.set_reference_clock(50_000_000);
//! // Optional DMA capability can be installed here before the protocol layer
//! // owns the host.
//!
//! let mut card = SdioSdmmc::new_host2(host);
//! let mut scratch = SdioInitScratch::new();
//! let mut request = card.submit_init(&mut scratch)?;
//! // Poll request here. Runtime code chooses spin, yield, IRQ wait, or timer.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! The runtime block queue adapter belongs in OS/platform glue. The reusable
//! driver crate exposes request state and host submit/poll primitives instead:
//!
//! ```compile_fail
//! use dwmmc_host::BlockQueue;
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that
//! the supplied address is a valid, exclusively-owned DW_mshc
//! register file.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::sync::Arc;
use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

use log::warn;

mod command;
mod dma;
mod host;
pub mod rdif;
mod regs;

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

use crate::regs::RegisterBlockVolatileFieldAccess;
pub use crate::{
    dma::{BlockRequest, BlockRequestSlot, IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, RequestId},
    host::{CardDetect, DEFAULT_FIFO_OFFSET, DwMmc, HostClock},
};

/// Stable controller event extracted from DW_mshc raw interrupt status.
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
    ResetAll(DwMmcResetState),
    ResetDataLine { started: bool, polls: u32 },
    PowerOn(DwMmcResetState),
    PowerOff,
    SetClock(DwMmcClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(SignalVoltage),
}

enum DwMmcResetState {
    Start,
    WaitReset { polls: u32 },
}

enum DwMmcClockState {
    Start {
        speed: Option<ClockSpeed>,
        target_hz: u32,
        wait_prvdata_complete: bool,
    },
    ExternalSetClock {
        speed: Option<ClockSpeed>,
        target_hz: u32,
        wait_prvdata_complete: bool,
    },
    WaitGate {
        polls: u32,
        target_hz: u32,
    },
    ProgramDivider {
        target_hz: u32,
    },
    WaitDivider {
        polls: u32,
    },
    Enable,
    WaitEnable {
        polls: u32,
    },
}

const DWMMC_RESET_POLLS: u32 = host::DWMMC_HW_POLL_LIMIT;
const DWMMC_CLOCK_POLLS: u32 = host::DWMMC_HW_POLL_LIMIT;

/// Owned DWMMC IRQ top-half endpoint.
pub struct DwMmcIrq {
    irq: Arc<host::IrqCore>,
}

pub(crate) const DWMMC_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const DWMMC_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const DWMMC_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const DWMMC_INT_TXDR: u32 = 1 << 4;
pub(crate) const DWMMC_INT_RXDR: u32 = 1 << 5;
pub(crate) const DWMMC_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const DWMMC_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const DWMMC_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const DWMMC_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const DWMMC_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const DWMMC_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const DWMMC_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
const DWMMC_IDMAC_INT_TI: u32 = 1 << 0;
const DWMMC_IDMAC_INT_RI: u32 = 1 << 1;
const DWMMC_IDMAC_INT_NI: u32 = 1 << 8;
pub(crate) const DWMMC_INT_START_BIT_ERROR: u32 = 1 << 13;
pub(crate) const DWMMC_INT_END_BIT_ERROR: u32 = 1 << 15;
pub(crate) const DWMMC_INT_ERROR_MASK: u32 = DWMMC_INT_RESPONSE_ERROR
    | DWMMC_INT_RESPONSE_CRC_ERROR
    | DWMMC_INT_DATA_CRC_ERROR
    | DWMMC_INT_RESPONSE_TIMEOUT
    | DWMMC_INT_DATA_READ_TIMEOUT
    | DWMMC_INT_HOST_TIMEOUT
    | DWMMC_INT_FIFO_UNDER_OVER_RUN
    | DWMMC_INT_HARDWARE_LOCKED_WRITE
    | DWMMC_INT_START_BIT_ERROR
    | DWMMC_INT_END_BIT_ERROR;

impl ProtocolSdioHost for DwMmc {
    type Event = Event;
    type DataRequest<'a> = DataRequest<'a>;
    type BusRequest = ReadyBusRequest;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        self.check_not_poisoned()?;
        DwMmc::submit_command(self, cmd)
    }

    fn poll_command_response(&mut self) -> Result<sdmmc_protocol::CommandResponsePoll, Error> {
        DwMmc::poll_command_response(self)
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
        self.set_card_type(width);
        Ok(())
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        let target_hz = clock_hz_for_speed(speed);
        if self.ext_clock.is_some() {
            let clock = self.ext_clock.take().ok_or(Error::InvalidArgument)?;
            let result = clock.set_clock(target_hz);
            self.ext_clock = Some(clock);
            let bus_hz = result?;
            self.set_reference_clock(bus_hz);
        }
        self.set_uhs_timing(speed);
        self.program_clock(target_hz)
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        self.set_signal_voltage(voltage)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        DwMmc::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        DwMmc::disable_completion_irq(self);
        Ok(())
    }

    fn completion_irq_enabled(&self) -> bool {
        DwMmc::completion_irq_enabled(self)
    }

    fn submit_bus_op(&mut self, op: SdioBusOp) -> Result<Self::BusRequest, Error> {
        submit_ready_bus_op(self, op)
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        poll_ready_bus_op(request)
    }
}

impl SdioIrqHost for DwMmc {
    type IrqHandle = DwMmcIrq;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        DwMmc::irq_endpoint(self)
    }
}

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

impl DwMmc {
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
            sdio_host2::BusOp::ResetAll => Ok(BusRequestState::ResetAll(DwMmcResetState::Start)),
            sdio_host2::BusOp::ResetCommandLine => Err(sdio_host2::Error::Unsupported),
            sdio_host2::BusOp::ResetDataLine => Ok(BusRequestState::ResetDataLine {
                started: false,
                polls: 0,
            }),
            sdio_host2::BusOp::PowerOn => Ok(BusRequestState::PowerOn(DwMmcResetState::Start)),
            sdio_host2::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdio_host2::BusOp::SetClock(speed) => {
                let target_hz = clock_hz_for_speed(speed);
                if target_hz == 0 {
                    return Err(sdio_host2::Error::Unsupported);
                }
                Ok(BusRequestState::SetClock(DwMmcClockState::Start {
                    speed: Some(speed),
                    target_hz,
                    wait_prvdata_complete: true,
                }))
            }
            sdio_host2::BusOp::SetClockHz(sdio_host2::ClockHz(hz)) => {
                Ok(BusRequestState::SetClock(DwMmcClockState::Start {
                    speed: None,
                    target_hz: hz,
                    wait_prvdata_complete: true,
                }))
            }
            sdio_host2::BusOp::SetBusWidth(width) => match width {
                BusWidth::Bit1 | BusWidth::Bit4 | BusWidth::Bit8 => {
                    Ok(BusRequestState::SetBusWidth(width))
                }
                _ => Err(sdio_host2::Error::Unsupported),
            },
            sdio_host2::BusOp::SetSignalVoltage(voltage) => match volt_mask_for_signal(voltage) {
                Ok(_) => Ok(BusRequestState::SetSignalVoltage(voltage)),
                Err(err) => Err(map_protocol_error(err)),
            },
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
            BusRequestState::PowerOn(reset) => self.poll_host2_power_on(reset),
            BusRequestState::PowerOff => {
                self.regs.pwren().write(0);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.poll_host2_clock(clock),
            BusRequestState::SetBusWidth(width) => {
                self.set_card_type(*width);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => {
                self.set_signal_voltage(*voltage)
                    .map_err(map_protocol_error)?;
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
        }
    }

    fn poll_host2_reset_all(
        &mut self,
        state: &mut DwMmcResetState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            DwMmcResetState::Start => {
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
                *state = DwMmcResetState::WaitReset { polls: 0 };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcResetState::WaitReset { polls } => {
                let ctrl = self.regs.ctrl().read();
                if !ctrl.controller_reset() && !ctrl.fifo_reset() && !ctrl.dma_reset() {
                    self.regs.intmask().write(0);
                    self.clear_all_int_status();
                    self.irq.state.clear(u32::MAX);
                    self.completion_irq_enabled
                        .store(false, core::sync::atomic::Ordering::Release);
                    self.regs.ctype().write(crate::regs::CType::new());
                    self.regs.uhs().write(crate::regs::UHS::new());
                    self.program_linux_init_baseline();
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                if *polls >= DWMMC_RESET_POLLS {
                    self.log_host2_timeout("reset-all");
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                        Phase::Init,
                    ))));
                }
                *polls += 1;
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    fn poll_host2_power_on(
        &mut self,
        state: &mut DwMmcResetState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        if matches!(state, DwMmcResetState::Start) {
            self.regs.pwren().write(1);
        }
        self.poll_host2_reset_all(state)
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
        if *polls >= DWMMC_RESET_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::DataRead,
            ))));
        }
        *polls += 1;
        Ok(sdio_host2::RequestPoll::Pending)
    }

    fn poll_host2_clock(
        &mut self,
        state: &mut DwMmcClockState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            DwMmcClockState::Start {
                speed,
                target_hz,
                wait_prvdata_complete,
            } => {
                if self.ext_clock.is_some() {
                    *state = DwMmcClockState::ExternalSetClock {
                        speed: *speed,
                        target_hz: *target_hz,
                        wait_prvdata_complete: *wait_prvdata_complete,
                    };
                    return Ok(sdio_host2::RequestPoll::Pending);
                }
                if let Some(speed) = *speed {
                    self.set_uhs_timing(speed);
                }
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.clksrc().write(0);
                self.start_update_clock(false, *wait_prvdata_complete);
                *state = DwMmcClockState::WaitGate {
                    polls: 0,
                    target_hz: *target_hz,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::ExternalSetClock {
                speed,
                target_hz,
                wait_prvdata_complete,
            } => {
                let clock = self.ext_clock.take().ok_or(sdio_host2::Error::Controller)?;
                let result = clock.set_clock(*target_hz);
                self.ext_clock = Some(clock);
                let bus_hz = result.map_err(map_protocol_error)?;
                self.set_reference_clock(bus_hz);
                if let Some(speed) = *speed {
                    self.set_uhs_timing(speed);
                }
                self.regs.clkena().write(crate::regs::ClkEna::new());
                self.regs.clksrc().write(0);
                self.start_update_clock(false, *wait_prvdata_complete);
                *state = DwMmcClockState::WaitGate {
                    polls: 0,
                    target_hz: *target_hz,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::WaitGate { polls, target_hz } => {
                if self.poll_update_clock_complete(polls)? {
                    *state = DwMmcClockState::ProgramDivider {
                        target_hz: *target_hz,
                    };
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::ProgramDivider { target_hz } => {
                let div = dwmmc_clock_divisor(self.ref_clock_hz, *target_hz);
                self.regs
                    .clkdiv()
                    .write(crate::regs::ClkDiv::new().with_clk_divider0(div));
                self.start_update_clock(false, true);
                *state = DwMmcClockState::WaitDivider { polls: 0 };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::WaitDivider { polls } => {
                if self.poll_update_clock_complete(polls)? {
                    *state = DwMmcClockState::Enable;
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::Enable => {
                self.regs
                    .clkena()
                    .write(crate::regs::ClkEna::new().with_cclk_enable(1));
                self.start_update_clock(false, true);
                *state = DwMmcClockState::WaitEnable { polls: 0 };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            DwMmcClockState::WaitEnable { polls } => {
                if self.poll_update_clock_complete(polls)? {
                    return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    fn start_update_clock(&self, voltage_switch: bool, wait_prvdata_complete: bool) {
        self.regs.cmd().write(
            crate::regs::Cmd::new()
                .with_start_cmd(true)
                .with_use_hold_reg(false)
                .with_wait_prvdata_complete(wait_prvdata_complete)
                .with_update_clock_registers_only(true)
                .with_volt_switch(voltage_switch),
        );
    }

    fn poll_update_clock_complete(&self, polls: &mut u32) -> Result<bool, sdio_host2::Error> {
        if !self.regs.cmd().read().start_cmd() {
            return Ok(true);
        }
        if *polls >= DWMMC_CLOCK_POLLS {
            self.log_host2_timeout("clock-update");
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        *polls += 1;
        Ok(false)
    }

    fn log_host2_timeout(&self, op: &str) {
        warn!(
            "dwmmc-host2: {op} timeout ctrl={:#010x} cmd={:#010x} status={:#010x} \
             rintsts={:#010x} mintsts={:#010x} intmask={:#010x} clkena={:#010x} clksrc={:#010x} \
             clkdiv={:#010x} ctype={:#010x} pwren={:#010x} fifoth={:#010x} tmout={:#010x}",
            self.regs.ctrl().read().into_bits(),
            self.regs.cmd().read().into_bits(),
            self.regs.status().read().into_bits(),
            self.regs.rintsts().read().into_bits(),
            self.regs.mintsts().read(),
            self.regs.intmask().read(),
            self.regs.clkena().read().into_bits(),
            self.regs.clksrc().read(),
            self.regs.clkdiv().read().into_bits(),
            self.regs.ctype().read().into_bits(),
            self.regs.pwren().read(),
            self.regs.fifoth().read(),
            self.regs.tmout().read(),
        );
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
                self.reset_fifo().map_err(map_protocol_error)?;
            }
            BusRequestState::PowerOn(_)
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
    host: &mut DwMmc,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if !host.card_present() {
        return Err(Error::NoCard);
    }
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
    host: &mut DwMmc,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if !host.card_present() {
        return Err(Error::NoCard);
    }
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

pub(crate) fn event_from_raw_status(raw_status: u32) -> Event {
    let status = crate::regs::RIntSts::from_bits(raw_status);
    if raw_status == 0 {
        Event::None
    } else if status.error() {
        Event::Error { raw_status }
    } else if status.command_done() {
        Event::CommandComplete
    } else if status.data_transfer_over() {
        Event::TransferComplete
    } else if status.receive_fifo_data_request() {
        Event::ReceiveReady
    } else if status.transmit_fifo_data_request() {
        Event::TransmitReady
    } else {
        Event::Other { raw_status }
    }
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

    fn queue_id(&self) -> Option<BlockRequestId> {
        match self {
            Event::TransferComplete | Event::ReceiveReady | Event::TransmitReady => {
                Some(BlockRequestId::new(0))
            }
            Event::None | Event::CommandComplete | Event::Error { .. } | Event::Other { .. } => {
                None
            }
        }
    }
}

impl DwMmc {
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

    pub fn irq_endpoint(&mut self) -> DwMmcIrq {
        DwMmcIrq {
            irq: self.irq.clone(),
        }
    }

    /// Read and acknowledge pending controller status, returning a stable
    /// event for OS glue to translate into wakeups or worker scheduling.
    pub fn handle_irq(&mut self) -> Event {
        handle_irq_core(&self.irq)
    }
}

impl SdioIrqHandle for DwMmcIrq {
    type Event = Event;

    fn handle_irq(&mut self) -> Self::Event {
        handle_irq_core(&self.irq)
    }
}

fn handle_irq_core(irq: &host::IrqCore) -> Event {
    let generation = irq.state.generation();
    let mut raw_status = irq.regs.mintsts().read();
    if raw_status != 0 {
        irq.regs
            .rintsts()
            .write(crate::regs::RIntSts::from_bits(raw_status));
    }
    let idmac_status = irq.regs.idsts().read();
    if idmac_status & (DWMMC_IDMAC_INT_TI | DWMMC_IDMAC_INT_RI) != 0 {
        irq.regs
            .idsts()
            .write(DWMMC_IDMAC_INT_TI | DWMMC_IDMAC_INT_RI);
        irq.regs.idsts().write(DWMMC_IDMAC_INT_NI);
        raw_status |= crate::DWMMC_INT_DATA_TRANSFER_OVER;
    }
    irq.state.cache_if_current(generation, raw_status);
    event_from_raw_status(raw_status)
}

fn clock_hz_for_speed(speed: ClockSpeed) -> u32 {
    match speed {
        ClockSpeed::Identification => 400_000,
        ClockSpeed::Default | ClockSpeed::Sdr12 => 25_000_000,
        ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => 50_000_000,
        ClockSpeed::Sdr50 | ClockSpeed::Ddr50 => 50_000_000,
        ClockSpeed::Sdr104 => 104_000_000,
        ClockSpeed::Hs200 => 200_000_000,
        // Future ClockSpeed variants: unknown frequency, signal 0.
        _ => 0,
    }
}

fn dwmmc_clock_divisor(ref_clock_hz: u32, target_hz: u32) -> u8 {
    if ref_clock_hz == 0 || target_hz == 0 || target_hz >= ref_clock_hz {
        0
    } else {
        ref_clock_hz.div_ceil(2 * target_hz).min(0xFF) as u8
    }
}

pub(crate) fn ddr_mask_for_speed(speed: ClockSpeed) -> u16 {
    match speed {
        ClockSpeed::Ddr50 => 1,
        _ => 0,
    }
}

pub(crate) fn volt_mask_for_signal(voltage: SignalVoltage) -> Result<u16, Error> {
    match voltage {
        SignalVoltage::V330 => Ok(0),
        SignalVoltage::V180 => Ok(1),
        SignalVoltage::V120 => Err(Error::UnsupportedCommand),
        // Future SignalVoltage variants are not supported by this controller.
        _ => Err(Error::UnsupportedCommand),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UhsBits {
    pub ddr: u16,
    pub volt: u16,
}

pub(crate) fn uhs_bits_after_speed(cur: UhsBits, speed: ClockSpeed) -> UhsBits {
    UhsBits {
        ddr: ddr_mask_for_speed(speed),
        ..cur
    }
}

pub(crate) fn uhs_bits_after_voltage(
    cur: UhsBits,
    voltage: SignalVoltage,
) -> Result<UhsBits, Error> {
    Ok(UhsBits {
        volt: volt_mask_for_signal(voltage)?,
        ..cur
    })
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU32};

    use sdio_host2::ResponseType;

    use super::*;

    #[test]
    fn event_reports_command_completion_without_os_wakeup_policy() {
        let raw = crate::regs::RIntSts::new()
            .with_command_done(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::CommandComplete);
    }

    #[test]
    fn event_reports_transfer_completion_without_os_wakeup_policy() {
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::TransferComplete);
    }

    #[test]
    fn event_reports_error_status_without_translating_to_os_action() {
        let raw = crate::regs::RIntSts::new()
            .with_response_timeout(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::Error { raw_status: raw });
    }

    #[test]
    fn event_reports_data_completion_source_for_runtime_wakeup() {
        use sdmmc_protocol::sdio::{HostEvent, HostEventKind, HostEventSource};

        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();
        let event = event_from_raw_status(raw);

        assert_eq!(event.kind(), HostEventKind::TransferComplete);
        assert_eq!(event.source(), HostEventSource::Data);
        assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
    }

    #[test]
    fn exposes_block_buffer_constraints() {
        let host = unsafe { DwMmc::new_from_addr(0x1000_0000) };

        let dma = host.block_buffer_config(BlockTransferMode::Dma);
        assert_eq!(dma.block_size.get(), 512);
        assert_eq!(dma.align, 512);
        assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
    }

    #[test]
    fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
        let mut host = unsafe { DwMmc::new_from_addr(0x1000_0000) };
        host.command_state = command::CommandState::Issued {
            cmd: Command::new(0, 0, ResponseType::None),
            polls: 0,
        };
        let mut buf = [0u8; 512];
        let data = sdio_host2::DataPhase::read(
            NonZeroU16::new(512).unwrap(),
            NonZeroU32::new(1).unwrap(),
            &mut buf,
        )
        .unwrap();
        let tx = sdio_host2::Transaction::with_data(Command::new(17, 0, ResponseType::R1), data);

        let err =
            match unsafe { <DwMmc as sdio_host2::SdioHost>::submit_transaction(&mut host, tx) } {
                Ok(_) => panic!("busy host accepted a second transaction"),
                Err(err) => err,
            };

        assert_eq!(err, sdio_host2::Error::Busy);
        assert!(host.pending_data.is_none());
        assert_eq!(host.data_blocks_remaining, 0);
    }

    #[test]
    fn owned_irq_endpoint_acks_and_caches_status() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.irq.state.begin_request();
        let old_generation = host.irq.state.generation();
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();
        const MINTSTS_WORD: usize = 16;
        unsafe {
            mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(raw);
        }

        let mut irq = host.irq_endpoint();

        assert_eq!(irq.handle_irq(), Event::TransferComplete);
        assert_eq!(host.irq.state.pending(), raw);
        unsafe {
            mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(0);
        }
        assert_eq!(host.handle_irq(), Event::None);

        host.irq.state.end_request();
        host.irq.state.begin_request();
        assert_ne!(host.irq.state.generation(), old_generation);
        host.irq
            .state
            .cache_if_current(old_generation, crate::DWMMC_INT_DATA_TRANSFER_OVER);
        assert_eq!(host.irq.state.pending(), 0);
    }

    #[test]
    fn idmac_irq_completion_is_cached_as_data_completion() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const IDSTS_WORD: usize = 35;
        host.irq.state.begin_request();
        unsafe {
            mmio.as_mut_ptr()
                .add(IDSTS_WORD)
                .write_volatile(DWMMC_IDMAC_INT_TI);
        }

        let mut irq = host.irq_endpoint();

        assert_eq!(irq.handle_irq(), Event::TransferComplete);
        assert_eq!(host.irq.state.pending(), DWMMC_INT_DATA_TRANSFER_OVER);
        let cleared = unsafe { mmio.as_ptr().add(IDSTS_WORD).read_volatile() };
        assert_eq!(cleared, DWMMC_IDMAC_INT_NI);
    }

    #[test]
    fn completion_irq_uses_dma_mask_until_fifo_path_requests_fifo_irqs() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const INTMASK_WORD: usize = 9;
        let dma_mask = crate::DWMMC_INT_DATA_TRANSFER_OVER
            | crate::DWMMC_INT_COMMAND_DONE
            | crate::DWMMC_INT_ERROR_MASK;
        let fifo_mask = dma_mask | crate::DWMMC_INT_RXDR | crate::DWMMC_INT_TXDR;

        host.enable_completion_irq();

        let intmask = unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() };
        assert_eq!(intmask, dma_mask);

        host.program_fifo_interrupt_mask();

        let intmask = unsafe { mmio.as_ptr().add(INTMASK_WORD).read_volatile() };
        assert_eq!(intmask, fifo_mask);
    }

    #[test]
    fn clear_all_int_status_matches_linux_w1c_all_bits() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let host = unsafe { DwMmc::new(base) };
        const RINTSTS_WORD: usize = 17;
        unsafe {
            mmio.as_mut_ptr()
                .add(RINTSTS_WORD)
                .write_volatile(crate::DWMMC_INT_COMMAND_DONE);
        }

        host.clear_all_int_status();

        let written = unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() };
        assert_eq!(written, u32::MAX);
    }

    #[test]
    fn host2_reset_programs_linux_baseline_without_clock_update() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::ResetAll)
        }
        .unwrap();
        const CTRL_WORD: usize = 0;
        const TMOUT_WORD: usize = 5;
        const CMD_WORD: usize = 11;
        const RINTSTS_WORD: usize = 17;
        const FIFOTH_WORD: usize = 19;
        const EXPECTED_FIFOTH: u32 = (0x2 << 28) | (0x7f << 16) | 0x80;

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Pending
        ));
        unsafe {
            mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
        }
        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Ready(Ok(()))
        ));

        assert_eq!(
            unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() },
            u32::MAX
        );
        assert_eq!(
            unsafe { mmio.as_ptr().add(TMOUT_WORD).read_volatile() },
            u32::MAX
        );
        assert_eq!(
            unsafe { mmio.as_ptr().add(FIFOTH_WORD).read_volatile() },
            EXPECTED_FIFOTH
        );
        assert_eq!(unsafe { mmio.as_ptr().add(CMD_WORD).read_volatile() }, 0);
    }

    #[test]
    fn host2_power_on_resets_after_enabling_pwren() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::PowerOn)
        }
        .unwrap();
        const CTRL_WORD: usize = 0;
        const PWREN_WORD: usize = 1;
        const TMOUT_WORD: usize = 5;
        const RINTSTS_WORD: usize = 17;

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Pending
        ));
        assert_eq!(unsafe { mmio.as_ptr().add(PWREN_WORD).read_volatile() }, 1);
        let ctrl =
            crate::regs::Ctrl::from_bits(unsafe { mmio.as_ptr().add(CTRL_WORD).read_volatile() });
        assert!(ctrl.controller_reset());
        assert!(ctrl.fifo_reset());
        assert!(ctrl.dma_reset());

        unsafe {
            mmio.as_mut_ptr().add(CTRL_WORD).write_volatile(0);
        }
        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Ready(Ok(()))
        ));
        assert_eq!(
            unsafe { mmio.as_ptr().add(RINTSTS_WORD).read_volatile() },
            u32::MAX
        );
        assert_eq!(
            unsafe { mmio.as_ptr().add(TMOUT_WORD).read_volatile() },
            u32::MAX
        );
    }

    #[test]
    fn absent_controller_card_detect_rejects_command_before_issue() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        const CMD_WORD: usize = 11;
        const CDETECT_WORD: usize = 20;
        unsafe {
            mmio.as_mut_ptr().add(CDETECT_WORD).write_volatile(1);
        }

        let err = host
            .submit_command(&Command::new(8, 0x1aa, ResponseType::R7))
            .expect_err("absent card must not issue a command");

        assert_eq!(err, Error::NoCard);
        assert_eq!(unsafe { mmio.as_ptr().add(CMD_WORD).read_volatile() }, 0);
        assert!(matches!(host.command_state, command::CommandState::Idle));
    }

    #[test]
    fn host2_set_clock_rewrites_clksrc_like_linux_setup_bus() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.set_reference_clock(50_000_000);
        const CLKSRC_WORD: usize = 3;
        unsafe {
            mmio.as_mut_ptr()
                .add(CLKSRC_WORD)
                .write_volatile(0xdead_beef);
        }
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetClock(sdio_host2::ClockSpeed::Identification),
            )
        }
        .unwrap();

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Pending
        ));

        assert_eq!(unsafe { mmio.as_ptr().add(CLKSRC_WORD).read_volatile() }, 0);
    }

    #[test]
    fn host2_external_clock_returned_bus_hz_feeds_dwmmc_divider() {
        struct Clock;

        impl HostClock for Clock {
            fn set_clock(&self, target_hz: u32) -> Result<u32, Error> {
                assert_eq!(target_hz, 400_000);
                Ok(400_000)
            }
        }

        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.set_reference_clock(50_000_000);
        host.set_external_clock(Clock);
        let mut request = unsafe {
            <DwMmc as sdio_host2::SdioHost>::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetClock(sdio_host2::ClockSpeed::Identification),
            )
        }
        .unwrap();
        const CMD_WORD: usize = 11;
        const CLKDIV_WORD: usize = 2;

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Pending
        ));
        assert_eq!(host.reference_clock(), 50_000_000);

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Pending
        ));
        assert_eq!(host.reference_clock(), 400_000);
        unsafe {
            mmio.as_mut_ptr().add(CMD_WORD).write_volatile(0);
        }

        assert!(matches!(
            <DwMmc as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request).unwrap(),
            sdio_host2::RequestPoll::Pending
        ));
        assert_eq!(unsafe { mmio.as_ptr().add(CLKDIV_WORD).read_volatile() }, 0);
    }

    #[test]
    fn rintsts_error_includes_host_timeout_and_fifo_overrun() {
        assert!(crate::regs::RIntSts::new().with_host_timeout(true).error());
        assert!(
            crate::regs::RIntSts::new()
                .with_fifo_under_over_run(true)
                .error()
        );
    }

    #[test]
    fn uhs_i_sdr_modes_keep_ddr_disabled() {
        let cur = UhsBits { ddr: 1, volt: 1 };

        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr50).ddr, 0);
        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr104).ddr, 0);
        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Hs200).ddr, 0);
    }

    #[test]
    fn ddr50_enables_ddr_mode_for_card0() {
        let cur = UhsBits { ddr: 0, volt: 1 };

        assert_eq!(
            uhs_bits_after_speed(cur, ClockSpeed::Ddr50),
            UhsBits { ddr: 1, volt: 1 }
        );
    }

    #[test]
    fn uhs_i_voltage_switch_selects_1v8_for_card0() {
        let cur = UhsBits { ddr: 1, volt: 0 };

        assert_eq!(
            uhs_bits_after_voltage(cur, SignalVoltage::V180).unwrap(),
            UhsBits { ddr: 1, volt: 1 }
        );
        assert_eq!(
            uhs_bits_after_voltage(cur, SignalVoltage::V330).unwrap(),
            UhsBits { ddr: 1, volt: 0 }
        );
    }

    #[test]
    fn unsupported_1v2_voltage_is_rejected() {
        assert_eq!(
            volt_mask_for_signal(SignalVoltage::V120).unwrap_err(),
            Error::UnsupportedCommand
        );
    }

    #[test]
    fn data_command_index_is_recorded_for_diagnostics() {
        let mut host = unsafe { DwMmc::new_from_addr(0x1000_0000) };
        host.data_cmd_index = 6;

        assert_eq!(host.data_cmd_index, 6);
    }
}
