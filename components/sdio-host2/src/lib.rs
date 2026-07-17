//! Physical SD/SDIO/MMC host-bus transaction traits.
//!
//! This crate intentionally models the shared CMD/DAT bus rather than a card,
//! block device, filesystem, or runtime queue. A host accepts one transaction
//! at a time: a command, an optional data phase, and a bounded state-advance
//! path. Runtime integrations call that path only after publishing an IRQ
//! event; initialization may additionally re-enter it at an explicit absolute
//! deadline. Higher-level SD/MMC card protocols live in `sdmmc-protocol`.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;
use core::{
    fmt,
    num::{NonZeroU16, NonZeroU32},
};

use dma_api::{CompletedDma, CpuDmaBuffer, DmaDirection, PreparedDma};

/// SD/SDIO/MMC command packet submitted on the CMD line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    pub index: u8,
    pub argument: u32,
    pub response: ResponseType,
}

impl Command {
    pub const fn new(index: u8, argument: u32, response: ResponseType) -> Self {
        Self {
            index,
            argument,
            response,
        }
    }

    pub const fn index(self) -> u8 {
        self.index
    }

    pub const fn argument(self) -> u32 {
        self.argument
    }

    pub const fn with_response(self, response: ResponseType) -> Self {
        Self { response, ..self }
    }

    /// Return a copy of this command with its response type overridden.
    ///
    /// Kept as a compatibility alias for existing SD/MMC protocol helpers.
    pub const fn with_resp_type(self, response: ResponseType) -> Self {
        self.with_response(response)
    }

    /// Compatibility alias for older SD/MMC command helpers.
    pub const fn cmd(self) -> u8 {
        self.index
    }

    /// Compatibility alias for older SD/MMC command helpers.
    pub const fn arg(self) -> u32 {
        self.argument
    }

    /// Direction of the data phase that follows this command when it is
    /// unambiguous from the command index alone.
    ///
    /// SDIO CMD53 carries its direction in the argument; CMD6 is also
    /// overloaded between ACMD6 and SWITCH_FUNC, so both return `None`.
    pub const fn data_direction(&self) -> Option<DataDirection> {
        match self.index {
            17 | 18 => Some(DataDirection::Read),
            24 | 25 => Some(DataDirection::Write),
            _ => None,
        }
    }

    /// Size in bytes of the data block when fixed by the command index.
    pub const fn data_block_size(&self) -> Option<u32> {
        match self.index {
            17 | 18 | 24 | 25 => Some(512),
            _ => None,
        }
    }

    /// Compute the SD SPI-mode CRC7 for this command packet.
    pub fn crc7(&self) -> u8 {
        let mut crc: u8 = 0;
        let token: u8 = 0x40 | (self.index & 0x3F);
        crc = crc7_update(crc, token);
        for byte in self.argument.to_be_bytes() {
            crc = crc7_update(crc, byte);
        }
        (crc << 1) | 1
    }

    /// Build the 6-byte SD SPI command packet.
    pub fn to_spi_bytes(&self) -> [u8; 6] {
        let crc = self.crc7();
        let token = 0x40 | (self.index & 0x3F);
        let arg = self.argument.to_be_bytes();
        [token, arg[0], arg[1], arg[2], arg[3], crc]
    }
}

fn crc7_update(crc: u8, byte: u8) -> u8 {
    let mut crc = crc;
    let mut data = byte;
    for _ in 0..8 {
        crc <<= 1;
        if (crc ^ data) & 0x80 != 0 {
            crc ^= 0x89;
        }
        data <<= 1;
    }
    crc
}

/// Command response shape expected from the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResponseType {
    None,
    R1,
    R1b,
    R2,
    R3,
    R4,
    R5,
    R6,
    R7,
}

/// Raw response words harvested by a host controller.
///
/// ABI:
///
/// - 48-bit responses store their response payload in `words[0]`.
/// - R2/CID/CSD responses store four 32-bit words in most-significant-word
///   first order.
/// - Each word is the big-endian value of the corresponding response bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawResponse {
    pub ty: ResponseType,
    pub words: [u32; 4],
}

impl RawResponse {
    pub const fn new(ty: ResponseType, words: [u32; 4]) -> Self {
        Self { ty, words }
    }

    pub const fn empty() -> Self {
        Self {
            ty: ResponseType::None,
            words: [0; 4],
        }
    }
}

/// Direction of a data phase on DAT lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DataDirection {
    Read,
    Write,
}

/// Caller-owned data buffer tied to an in-flight transaction lifetime.
pub enum DataBuffer<'a> {
    Read(&'a mut [u8]),
    Write(&'a [u8]),
    /// CPU-owned backing retained by an interrupt-driven PIO request.
    OwnedCpu(CpuDmaBuffer),
    Dma(PreparedDma),
}

impl DataBuffer<'_> {
    pub fn len(&self) -> usize {
        match self {
            Self::Read(buf) => buf.len(),
            Self::Write(buf) => buf.len(),
            Self::OwnedCpu(buffer) => buffer.len().get(),
            Self::Dma(buffer) => buffer.len().get(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn matches_direction(&self, direction: DataDirection) -> bool {
        match self {
            Self::Read(_) => direction == DataDirection::Read,
            Self::Write(_) => direction == DataDirection::Write,
            Self::OwnedCpu(buffer) => matches!(
                (buffer.direction(), direction),
                (DmaDirection::FromDevice, DataDirection::Read)
                    | (DmaDirection::ToDevice, DataDirection::Write)
                    | (DmaDirection::Bidirectional, _)
            ),
            Self::Dma(buffer) => matches!(
                (buffer.direction(), direction),
                (DmaDirection::FromDevice, DataDirection::Read)
                    | (DmaDirection::ToDevice, DataDirection::Write)
                    | (DmaDirection::Bidirectional, _)
            ),
        }
    }
}

pub type DataTransfer<'a> = DataBuffer<'a>;

/// Error returned while constructing an owned-DMA data phase.
pub struct DmaPhaseError {
    error: Error,
    buffer: Box<PreparedDma>,
}

impl DmaPhaseError {
    fn new(error: Error, buffer: PreparedDma) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub const fn error(&self) -> Error {
        self.error
    }

    pub fn into_buffer(self) -> PreparedDma {
        *self.buffer
    }

    pub fn into_parts(self) -> (Error, PreparedDma) {
        (self.error, *self.buffer)
    }
}

impl fmt::Debug for DmaPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DmaPhaseError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for DmaPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl core::error::Error for DmaPhaseError {}

/// Error returned while constructing an owned-CPU PIO data phase.
pub struct CpuPhaseError {
    error: Error,
    buffer: Box<CpuDmaBuffer>,
}

impl CpuPhaseError {
    fn new(error: Error, buffer: CpuDmaBuffer) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub const fn error(&self) -> Error {
        self.error
    }

    pub fn into_buffer(self) -> CpuDmaBuffer {
        *self.buffer
    }

    pub fn into_parts(self) -> (Error, CpuDmaBuffer) {
        (self.error, *self.buffer)
    }
}

impl fmt::Debug for CpuPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuPhaseError")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for CpuPhaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl core::error::Error for CpuPhaseError {}

/// Optional data phase associated with a command.
pub struct DataPhase<'a> {
    pub direction: DataDirection,
    pub block_size: NonZeroU16,
    pub block_count: NonZeroU32,
    pub buffer: DataBuffer<'a>,
}

impl<'a> DataPhase<'a> {
    pub fn read(
        block_size: NonZeroU16,
        block_count: NonZeroU32,
        buffer: &'a mut [u8],
    ) -> Result<Self, Error> {
        let phase = Self {
            direction: DataDirection::Read,
            block_size,
            block_count,
            buffer: DataBuffer::Read(buffer),
        };
        phase.validate()?;
        Ok(phase)
    }

    pub fn write(
        block_size: NonZeroU16,
        block_count: NonZeroU32,
        buffer: &'a [u8],
    ) -> Result<Self, Error> {
        let phase = Self {
            direction: DataDirection::Write,
            block_size,
            block_count,
            buffer: DataBuffer::Write(buffer),
        };
        phase.validate()?;
        Ok(phase)
    }

    pub fn dma(
        direction: DataDirection,
        block_size: NonZeroU16,
        block_count: NonZeroU32,
        buffer: PreparedDma,
    ) -> Result<Self, DmaPhaseError> {
        let phase = Self {
            direction,
            block_size,
            block_count,
            buffer: DataBuffer::Dma(buffer),
        };
        match phase.validate() {
            Ok(()) => Ok(phase),
            Err(err) => {
                let DataBuffer::Dma(buffer) = phase.buffer else {
                    unreachable!("DataPhase::dma always stores a DMA buffer")
                };
                Err(DmaPhaseError::new(err, buffer))
            }
        }
    }

    /// Build a PIO phase that transfers CPU-buffer ownership to the host.
    ///
    /// Unlike the borrowed read/write constructors, the resulting transaction
    /// may safely outlive its submit call. The host must return the same buffer
    /// through [`SdioHost::take_completed_cpu`] only after its command/FIFO
    /// engine is terminal or quiesced.
    pub fn owned_cpu(
        direction: DataDirection,
        block_size: NonZeroU16,
        block_count: NonZeroU32,
        buffer: CpuDmaBuffer,
    ) -> Result<Self, CpuPhaseError> {
        let phase = Self {
            direction,
            block_size,
            block_count,
            buffer: DataBuffer::OwnedCpu(buffer),
        };
        match phase.validate() {
            Ok(()) => Ok(phase),
            Err(err) => {
                let DataBuffer::OwnedCpu(buffer) = phase.buffer else {
                    unreachable!("DataPhase::owned_cpu always stores a CPU buffer")
                };
                Err(CpuPhaseError::new(err, buffer))
            }
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        let expected = usize::from(self.block_size.get())
            .checked_mul(
                usize::try_from(self.block_count.get()).map_err(|_| Error::InvalidArgument)?,
            )
            .ok_or(Error::InvalidArgument)?;
        if self.buffer.len() != expected {
            return Err(Error::InvalidArgument);
        }
        if !self.buffer.matches_direction(self.direction) {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }
}

/// One physical bus transaction: a command and an optional data phase.
pub struct Transaction<'a> {
    pub command: Command,
    pub data: Option<DataPhase<'a>>,
}

impl<'a> Transaction<'a> {
    pub const fn command(command: Command) -> Self {
        Self {
            command,
            data: None,
        }
    }

    pub const fn with_data(command: Command, data: DataPhase<'a>) -> Self {
        Self {
            command,
            data: Some(data),
        }
    }
}

/// Submit failure for an owned transaction.
///
/// When `transaction` is present, the caller may recover and retry the DMA
/// backing. When it is absent, the host had to consume/quiesce the transaction
/// while handling the error; no hardware access remains active on return.
pub struct SubmitTransactionError<'a> {
    pub error: Error,
    transaction: Option<Box<Transaction<'a>>>,
}

impl<'a> SubmitTransactionError<'a> {
    pub fn new(error: Error, transaction: Transaction<'a>) -> Self {
        Self {
            error,
            transaction: Some(Box::new(transaction)),
        }
    }

    pub const fn consumed(error: Error) -> Self {
        Self {
            error,
            transaction: None,
        }
    }

    pub fn into_transaction(self) -> Option<Transaction<'a>> {
        self.transaction.map(|transaction| *transaction)
    }
}

/// Result of advancing a submitted request once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestPoll<T> {
    Pending,
    Ready(Result<T, Error>),
}

/// Error returned when a request is polled through the wrong handle or after
/// its terminal state.
///
/// Unlike [`RequestPoll::Ready`], this is not a transfer terminal state for
/// the request payload. Implementations must not report a terminal
/// [`RequestPoll::Ready`] error until the controller is no longer accessing
/// the transaction buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PollRequestError {
    WrongOwner,
    WrongKind,
    AlreadyCompleted,
    StaleGeneration,
    /// Recovery could not be reported through the requested handle.
    ///
    /// Safe host implementations must still quiesce the hardware before any
    /// request object that borrows caller memory can be dropped. This variant
    /// is diagnostic only; it must not mean DMA is still active.
    RecoveryFailed,
}

/// SD/SDIO/MMC bus width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BusWidth {
    Bit1,
    Bit4,
    Bit8,
}

/// Named card clock modes used by SD/MMC protocol state machines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClockSpeed {
    Identification,
    Default,
    HighSpeed,
    Sdr12,
    Sdr25,
    Sdr50,
    Sdr104,
    Ddr50,
    Hs200,
}

/// Concrete clock frequency request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClockHz(pub u32);

/// Bus signaling voltage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignalVoltage {
    V330,
    V180,
    V120,
}

/// Non-data bus operation that may itself need asynchronous completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BusOp {
    ResetAll,
    ResetCommandLine,
    ResetDataLine,
    PowerOn,
    PowerOff,
    SetClock(ClockSpeed),
    SetClockHz(ClockHz),
    SetBusWidth(BusWidth),
    SetSignalVoltage(SignalVoltage),
    ExecuteTuning {
        command: Command,
        block_size: NonZeroU16,
    },
}

/// Host/bus-layer error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    Busy,
    Timeout,
    Crc,
    NoCard,
    Unsupported,
    InvalidArgument,
    Misaligned,
    Bus,
    Controller,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Busy => "host bus is busy",
            Self::Timeout => "host bus timeout",
            Self::Crc => "host bus CRC error",
            Self::NoCard => "no card present",
            Self::Unsupported => "operation is not supported",
            Self::InvalidArgument => "invalid host bus argument",
            Self::Misaligned => "misaligned host bus buffer",
            Self::Bus => "host bus error",
            Self::Controller => "host controller error",
        };
        f.write_str(s)
    }
}

impl core::error::Error for Error {}

impl fmt::Display for PollRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::WrongOwner => "request belongs to a different host",
            Self::WrongKind => "request was polled through the wrong operation kind",
            Self::AlreadyCompleted => "request has already completed",
            Self::StaleGeneration => "request generation is no longer active",
            Self::RecoveryFailed => "request recovery failed",
        };
        f.write_str(s)
    }
}

impl core::error::Error for PollRequestError {}

/// Physical SD/SDIO/MMC host bus.
///
/// The base contract is single active transaction: a host may reject a submit
/// with [`Error::Busy`] while another transaction or bus operation is active.
pub trait SdioHost {
    type TransactionRequest<'a>
    where
        Self: 'a;
    type BusRequest;

    /// Submit one CMD/DAT transaction.
    ///
    /// # Safety
    ///
    /// Callers must poll the returned request until [`RequestPoll::Ready`] or
    /// retain it until [`Self::abort_transaction`] reaches a terminal result.
    /// In particular, `Error::Busy` is not terminal and requires a later retry
    /// after controller quiescence. Until a terminal path runs, the host may
    /// still access the associated data buffer through DMA or FIFO PIO.
    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, Error>
    where
        Self: 'a;

    /// Submit one CMD/DAT transaction while preserving transaction ownership
    /// on submit-side failure when the host has not started hardware access.
    ///
    /// The default path is kept for legacy hosts. Native DMA users should
    /// override it so submit failure can return the original transaction.
    ///
    /// # Safety
    ///
    /// Same lifetime contract as [`Self::submit_transaction`].
    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        match unsafe { self.submit_transaction(transaction) } {
            Ok(request) => Ok(request),
            Err(error) => Err(SubmitTransactionError::consumed(error)),
        }
    }

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<RequestPoll<RawResponse>, PollRequestError>
    where
        Self: 'a;

    /// Abort a transaction.
    ///
    /// `Ok(())` is a terminal ownership transition. `Error::Busy` means the
    /// controller cannot yet prove quiescence: the caller must retain the
    /// request and retry only after the controller lifecycle has masked and
    /// synchronized IRQ delivery and stopped DMA. Any other error must be
    /// terminal with respect to borrowed memory, even when it reports that
    /// reset or reconstruction failed.
    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), Error>
    where
        Self: 'a;

    fn take_completed_dma<'a>(
        &mut self,
        _request: &mut Self::TransactionRequest<'a>,
    ) -> Option<CompletedDma>
    where
        Self: 'a,
    {
        None
    }

    /// Return CPU ownership after an owned PIO request is terminal.
    ///
    /// Hosts must return `None` while their command or FIFO engine can still
    /// consume or produce bytes for this request.
    fn take_completed_cpu<'a>(
        &mut self,
        _request: &mut Self::TransactionRequest<'a>,
    ) -> Option<CpuDmaBuffer>
    where
        Self: 'a,
    {
        None
    }

    /// Submit one non-data bus operation.
    ///
    /// # Safety
    ///
    /// The returned request must be polled until [`RequestPoll::Ready`] or
    /// passed to [`Self::abort_bus_op`] before being dropped.
    unsafe fn submit_bus_op(&mut self, op: BusOp) -> Result<Self::BusRequest, Error>;

    fn poll_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
    ) -> Result<RequestPoll<()>, PollRequestError>;

    /// Abort a bus operation.
    ///
    /// Like [`Self::abort_transaction`], returning from this method means the
    /// controller is no longer executing the operation even when the return
    /// value carries a diagnostic error.
    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), Error>;

    fn now_ms(&self) -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockHost {
        busy: bool,
    }

    #[derive(Debug)]
    struct MockTransactionRequest {
        response: RawResponse,
        pending_once: bool,
        done: bool,
    }

    #[derive(Debug)]
    struct MockBusRequest {
        pending_once: bool,
        done: bool,
    }

    impl SdioHost for MockHost {
        type TransactionRequest<'a>
            = MockTransactionRequest
        where
            Self: 'a;
        type BusRequest = MockBusRequest;

        unsafe fn submit_transaction<'a>(
            &mut self,
            transaction: Transaction<'a>,
        ) -> Result<Self::TransactionRequest<'a>, Error>
        where
            Self: 'a,
        {
            if self.busy {
                return Err(Error::Busy);
            }
            self.busy = true;
            Ok(MockTransactionRequest {
                response: RawResponse::new(transaction.command.response, [0x1234, 0, 0, 0]),
                pending_once: true,
                done: false,
            })
        }

        fn poll_transaction<'a>(
            &mut self,
            request: &mut Self::TransactionRequest<'a>,
        ) -> Result<RequestPoll<RawResponse>, PollRequestError>
        where
            Self: 'a,
        {
            if request.done {
                return Err(PollRequestError::AlreadyCompleted);
            }
            if request.pending_once {
                request.pending_once = false;
                return Ok(RequestPoll::Pending);
            }
            self.busy = false;
            request.done = true;
            Ok(RequestPoll::Ready(Ok(request.response)))
        }

        fn abort_transaction<'a>(
            &mut self,
            request: &mut Self::TransactionRequest<'a>,
        ) -> Result<(), Error>
        where
            Self: 'a,
        {
            request.done = true;
            self.busy = false;
            Ok(())
        }

        unsafe fn submit_bus_op(&mut self, _op: BusOp) -> Result<Self::BusRequest, Error> {
            if self.busy {
                return Err(Error::Busy);
            }
            self.busy = true;
            Ok(MockBusRequest {
                pending_once: false,
                done: false,
            })
        }

        fn poll_bus_op(
            &mut self,
            request: &mut Self::BusRequest,
        ) -> Result<RequestPoll<()>, PollRequestError> {
            if request.done {
                return Err(PollRequestError::AlreadyCompleted);
            }
            if request.pending_once {
                request.pending_once = false;
                return Ok(RequestPoll::Pending);
            }
            self.busy = false;
            request.done = true;
            Ok(RequestPoll::Ready(Ok(())))
        }

        fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), Error> {
            request.done = true;
            self.busy = false;
            Ok(())
        }
    }

    #[test]
    fn data_phase_validates_buffer_shape() {
        let mut read = [0u8; 1024];
        let block = NonZeroU16::new(512).unwrap();
        let phase = DataPhase::read(block, NonZeroU32::new(2).unwrap(), &mut read).unwrap();
        assert_eq!(phase.direction, DataDirection::Read);
        assert_eq!(phase.buffer.len(), 1024);
    }

    #[test]
    fn host_reports_busy_for_second_active_transaction() {
        let mut host = MockHost { busy: false };
        let cmd = Command::new(17, 0, ResponseType::R1);
        let mut request = unsafe { host.submit_transaction(Transaction::command(cmd)) }.unwrap();
        assert_eq!(
            unsafe { host.submit_transaction(Transaction::command(cmd)) }.unwrap_err(),
            Error::Busy
        );
        assert_eq!(
            host.poll_transaction(&mut request),
            Ok(RequestPoll::Pending)
        );
        assert!(matches!(
            host.poll_transaction(&mut request),
            Ok(RequestPoll::Ready(Ok(_)))
        ));
        assert_eq!(
            host.poll_transaction(&mut request),
            Err(PollRequestError::AlreadyCompleted)
        );
        assert!(unsafe { host.submit_transaction(Transaction::command(cmd)) }.is_ok());
    }

    #[test]
    fn bus_op_uses_same_single_active_contract() {
        let mut host = MockHost { busy: false };
        let _request = unsafe { host.submit_bus_op(BusOp::SetClock(ClockSpeed::Default)) }.unwrap();
        assert_eq!(
            unsafe { host.submit_bus_op(BusOp::SetBusWidth(BusWidth::Bit4)) }.unwrap_err(),
            Error::Busy
        );
    }

    #[test]
    fn abort_releases_single_active_contract() {
        let mut host = MockHost { busy: false };
        let cmd = Command::new(17, 0, ResponseType::R1);
        let mut request = unsafe { host.submit_transaction(Transaction::command(cmd)) }.unwrap();

        host.abort_transaction(&mut request).unwrap();

        assert!(unsafe { host.submit_transaction(Transaction::command(cmd)) }.is_ok());
        assert_eq!(
            host.poll_transaction(&mut request),
            Err(PollRequestError::AlreadyCompleted)
        );
    }
}
