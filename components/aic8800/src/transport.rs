//! One-outstanding-request SDIO transaction engine.

use alloc::vec::Vec;
use core::num::{NonZeroU16, NonZeroU32, NonZeroUsize};

use dma_api::{CpuDmaBuffer, DeviceDma, DmaDirection};
use sdmmc_protocol::sdio::host2::SdioHost2Timed;

use crate::wire::SDIO_BLOCK_SIZE;

const CMD52: u8 = 52;
const CMD53: u8 = 53;
const CMD52_WRITE: u32 = 1 << 31;
const CMD53_WRITE: u32 = 1 << 31;
const CMD53_BLOCK_MODE: u32 = 1 << 27;
const CMD53_INCREMENT: u32 = 1 << 26;

#[derive(Debug)]
pub(crate) enum SdioOperation {
    Bus(sdio_host2::BusOp),
    Command(sdio_host2::Command),
    ReadByte {
        function: u8,
        address: u32,
    },
    WriteByte {
        function: u8,
        address: u32,
        value: u8,
    },
    ReadBlocks {
        function: u8,
        address: u32,
        increment: bool,
        blocks: u16,
    },
    WriteBlocks {
        function: u8,
        address: u32,
        increment: bool,
        bytes: Vec<u8>,
    },
}

pub(crate) struct SdioCompletion {
    pub response: sdio_host2::RawResponse,
    pub bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum TransactionError {
    #[error("another SDIO operation is already active")]
    Busy,
    #[error("invalid SDIO operation")]
    InvalidOperation,
    #[error("SDIO host error: {0}")]
    Host(#[source] sdio_host2::Error),
    #[error("SDIO request handle error: {0}")]
    Poll(#[source] sdio_host2::PollRequestError),
    #[error("failed to allocate an owned SDIO transfer buffer: {0}")]
    Dma(#[source] dma_api::DmaError),
    #[error("terminal SDIO data request did not return CPU ownership")]
    MissingBuffer,
}

pub(crate) enum TransactionPoll {
    Pending { wake_at_ns: Option<u64> },
    Ready(SdioCompletion),
}

enum TransactionData {
    None,
    Read,
    Write,
}

enum ActiveRequest<H>
where
    H: SdioHost2Timed + 'static,
{
    Bus(H::BusRequest),
    Transaction {
        request: H::TransactionRequest<'static>,
        data: TransactionData,
    },
}

/// Serializes every CMD/DAT operation through one owner-local request slot.
pub(crate) struct SdioTransactionEngine<H>
where
    H: SdioHost2Timed + 'static,
{
    host: H,
    dma: DeviceDma,
    active: Option<ActiveRequest<H>>,
}

impl<H> SdioTransactionEngine<H>
where
    H: SdioHost2Timed + 'static,
{
    pub fn new(host: H, dma: DeviceDma) -> Self {
        Self {
            host,
            dma,
            active: None,
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn submit(&mut self, operation: SdioOperation) -> Result<(), TransactionError> {
        if self.active.is_some() {
            return Err(TransactionError::Busy);
        }
        let request = match operation {
            SdioOperation::Bus(operation) => {
                // SAFETY: `active` retains the unique request until a terminal
                // poll; this engine never submits a second request meanwhile.
                let request = unsafe { self.host.submit_bus_op(operation) }
                    .map_err(TransactionError::Host)?;
                ActiveRequest::Bus(request)
            }
            SdioOperation::Command(command) => self.submit_transaction(
                sdio_host2::Transaction::command(command),
                TransactionData::None,
            )?,
            SdioOperation::ReadByte { function, address } => {
                let command = sdio_host2::Command::new(
                    CMD52,
                    cmd52_argument(false, function, address, 0)?,
                    sdio_host2::ResponseType::R5,
                );
                self.submit_transaction(
                    sdio_host2::Transaction::command(command),
                    TransactionData::None,
                )?
            }
            SdioOperation::WriteByte {
                function,
                address,
                value,
            } => {
                let command = sdio_host2::Command::new(
                    CMD52,
                    cmd52_argument(true, function, address, value)?,
                    sdio_host2::ResponseType::R5,
                );
                self.submit_transaction(
                    sdio_host2::Transaction::command(command),
                    TransactionData::None,
                )?
            }
            SdioOperation::ReadBlocks {
                function,
                address,
                increment,
                blocks,
            } => {
                let blocks = NonZeroU16::new(blocks).ok_or(TransactionError::InvalidOperation)?;
                let length = usize::from(blocks.get())
                    .checked_mul(SDIO_BLOCK_SIZE)
                    .and_then(NonZeroUsize::new)
                    .ok_or(TransactionError::InvalidOperation)?;
                let buffer = CpuDmaBuffer::new_zero(
                    &self.dma,
                    length,
                    SDIO_BLOCK_SIZE,
                    DmaDirection::FromDevice,
                )
                .map_err(TransactionError::Dma)?;
                let data = sdio_host2::DataPhase::owned_cpu(
                    sdio_host2::DataDirection::Read,
                    NonZeroU16::new(SDIO_BLOCK_SIZE as u16).unwrap(),
                    NonZeroU32::new(u32::from(blocks.get())).unwrap(),
                    buffer,
                )
                .map_err(|error| TransactionError::Host(error.error()))?;
                let command = sdio_host2::Command::new(
                    CMD53,
                    cmd53_argument(false, function, address, increment, blocks.get())?,
                    sdio_host2::ResponseType::R5,
                );
                self.submit_transaction(
                    sdio_host2::Transaction::with_data(command, data),
                    TransactionData::Read,
                )?
            }
            SdioOperation::WriteBlocks {
                function,
                address,
                increment,
                bytes,
            } => {
                if bytes.is_empty() || !bytes.len().is_multiple_of(SDIO_BLOCK_SIZE) {
                    return Err(TransactionError::InvalidOperation);
                }
                let blocks = bytes.len() / SDIO_BLOCK_SIZE;
                let blocks =
                    u16::try_from(blocks).map_err(|_| TransactionError::InvalidOperation)?;
                let blocks = NonZeroU16::new(blocks).ok_or(TransactionError::InvalidOperation)?;
                let mut buffer = CpuDmaBuffer::new_zero(
                    &self.dma,
                    NonZeroUsize::new(bytes.len()).unwrap(),
                    SDIO_BLOCK_SIZE,
                    DmaDirection::ToDevice,
                )
                .map_err(TransactionError::Dma)?;
                buffer.copy_to_device_from_slice(&bytes);
                let data = sdio_host2::DataPhase::owned_cpu(
                    sdio_host2::DataDirection::Write,
                    NonZeroU16::new(SDIO_BLOCK_SIZE as u16).unwrap(),
                    NonZeroU32::new(u32::from(blocks.get())).unwrap(),
                    buffer,
                )
                .map_err(|error| TransactionError::Host(error.error()))?;
                let command = sdio_host2::Command::new(
                    CMD53,
                    cmd53_argument(true, function, address, increment, blocks.get())?,
                    sdio_host2::ResponseType::R5,
                );
                self.submit_transaction(
                    sdio_host2::Transaction::with_data(command, data),
                    TransactionData::Write,
                )?
            }
        };
        self.active = Some(request);
        Ok(())
    }

    fn submit_transaction(
        &mut self,
        transaction: sdio_host2::Transaction<'static>,
        data: TransactionData,
    ) -> Result<ActiveRequest<H>, TransactionError> {
        // SAFETY: all data transactions own their backing and `active` retains
        // the request until terminal completion. Command-only requests borrow
        // no caller memory.
        let request = unsafe { self.host.submit_transaction_owned(transaction) }
            .map_err(|error| TransactionError::Host(error.error))?;
        Ok(ActiveRequest::Transaction { request, data })
    }

    pub fn poll(&mut self, now_ns: u64) -> Result<TransactionPoll, TransactionError> {
        let mut active = self
            .active
            .take()
            .ok_or(TransactionError::InvalidOperation)?;
        match &mut active {
            ActiveRequest::Bus(request) => match self.host.poll_bus_op_at(request, now_ns) {
                Ok(sdio_host2::RequestPoll::Pending) => {
                    let wake_at_ns = self.host.bus_op_wake_at(request);
                    self.active = Some(active);
                    Ok(TransactionPoll::Pending { wake_at_ns })
                }
                Ok(sdio_host2::RequestPoll::Ready(Ok(()))) => {
                    Ok(TransactionPoll::Ready(SdioCompletion {
                        response: sdio_host2::RawResponse::empty(),
                        bytes: Vec::new(),
                    }))
                }
                Ok(sdio_host2::RequestPoll::Ready(Err(error))) => {
                    Err(TransactionError::Host(error))
                }
                Err(error) => {
                    // PollRequestError is diagnostic, not a terminal hardware
                    // transition. Retain the request so an explicit abort or
                    // later owner activation still owns every backing byte.
                    self.active = Some(active);
                    Err(TransactionError::Poll(error))
                }
            },
            ActiveRequest::Transaction { request, data } => {
                match self.host.poll_transaction_at(request, now_ns) {
                    Ok(sdio_host2::RequestPoll::Pending) => {
                        let wake_at_ns = self.host.transaction_wake_at(request);
                        self.active = Some(active);
                        Ok(TransactionPoll::Pending { wake_at_ns })
                    }
                    Ok(sdio_host2::RequestPoll::Ready(result)) => {
                        let response = result.map_err(TransactionError::Host)?;
                        let bytes = match data {
                            TransactionData::None => Vec::new(),
                            TransactionData::Read => {
                                let buffer = self
                                    .host
                                    .take_completed_cpu(request)
                                    .ok_or(TransactionError::MissingBuffer)?;
                                buffer.complete_for_cpu_all();
                                buffer.as_slice_cpu().to_vec()
                            }
                            TransactionData::Write => {
                                let _buffer = self
                                    .host
                                    .take_completed_cpu(request)
                                    .ok_or(TransactionError::MissingBuffer)?;
                                Vec::new()
                            }
                        };
                        Ok(TransactionPoll::Ready(SdioCompletion { response, bytes }))
                    }
                    Err(error) => {
                        // See the bus-request branch above. Dropping this
                        // request here could release an owned CPU buffer while
                        // the controller still has an active data phase.
                        self.active = Some(active);
                        Err(TransactionError::Poll(error))
                    }
                }
            }
        }
    }

    /// Attempts to terminate the one active request without losing ownership.
    ///
    /// A `Busy` result keeps the request installed and requires the lifecycle
    /// owner to mask/synchronize IRQ delivery and quiesce the controller before
    /// retrying. All other host results are terminal by the `sdio-host2`
    /// contract, so the request and its owned backing may then be released.
    pub fn abort_active(&mut self) -> Result<(), TransactionError> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        let result = match &mut active {
            ActiveRequest::Bus(request) => self.host.abort_bus_op(request),
            ActiveRequest::Transaction { request, .. } => self.host.abort_transaction(request),
        };
        if matches!(result, Err(sdio_host2::Error::Busy)) {
            self.active = Some(active);
        }
        result.map_err(TransactionError::Host)
    }
}

fn cmd52_argument(
    write: bool,
    function: u8,
    address: u32,
    value: u8,
) -> Result<u32, TransactionError> {
    if function > 7 || address > 0x1ffff {
        return Err(TransactionError::InvalidOperation);
    }
    Ok((if write { CMD52_WRITE } else { 0 })
        | (u32::from(function) << 28)
        | (address << 9)
        | u32::from(value))
}

fn cmd53_argument(
    write: bool,
    function: u8,
    address: u32,
    increment: bool,
    blocks: u16,
) -> Result<u32, TransactionError> {
    if function > 7 || address > 0x1ffff || blocks == 0 || blocks > 511 {
        return Err(TransactionError::InvalidOperation);
    }
    Ok((if write { CMD53_WRITE } else { 0 })
        | (u32::from(function) << 28)
        | CMD53_BLOCK_MODE
        | (if increment { CMD53_INCREMENT } else { 0 })
        | (address << 9)
        | u32::from(blocks))
}

pub(crate) fn r5_data(response: sdio_host2::RawResponse) -> Result<u8, TransactionError> {
    let value = response.words[0];
    if value & ((1 << 15) | (1 << 14) | (1 << 11) | (1 << 9) | (1 << 8)) != 0 {
        return Err(TransactionError::Host(sdio_host2::Error::Bus));
    }
    Ok(value as u8)
}
