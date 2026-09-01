use alloc::vec::Vec;
use core::{fmt, num::NonZeroU16, time::Duration};

use sdmmc_protocol::sdio::io::{AddressMode, FunctionNumber, IoAddress, TransferMode};
use thiserror::Error;
use zeroize::Zeroize;

/// Absolute value in the monotonic-clock domain supplied by the owner.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct MonotonicTime(u64);

impl MonotonicTime {
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    pub fn after(self, duration: Duration) -> Self {
        Self(
            self.0
                .saturating_add(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)),
        )
    }
}

/// Caller-owned entropy used by WPA state.
#[derive(Clone, Eq, PartialEq)]
pub struct Entropy([u8; 32]);

impl Entropy {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Entropy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Entropy([REDACTED])")
    }
}

impl Drop for Entropy {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Pairwise master key supplied through the Linux wireless ABI.
#[derive(Clone, Eq, PartialEq)]
pub struct Pmk([u8; 32]);

impl Pmk {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for Pmk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Pmk([REDACTED])")
    }
}

impl Drop for Pmk {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Opaque ownership token returned after the matching TX request is consumed.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct TxToken(u64);

impl TxToken {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Error category returned by the SDIO capability adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdioFailure {
    Busy,
    Timeout,
    Crc,
    NoCard,
    Unsupported,
    InvalidRequest,
    Bus,
    Aborted,
}

/// One operation executed exclusively by the SDIO capability adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SdioRequestKind {
    EnableFunction(FunctionNumber),
    SetBlockSize {
        function: FunctionNumber,
        block_size: NonZeroU16,
    },
    EnableFunctionInterrupt(FunctionNumber),
    ReadByte {
        function: FunctionNumber,
        address: IoAddress,
    },
    WriteByte {
        function: FunctionNumber,
        address: IoAddress,
        value: u8,
        read_after_write: bool,
    },
    Read {
        function: FunctionNumber,
        address: IoAddress,
        address_mode: AddressMode,
        transfer_mode: TransferMode,
        length: usize,
    },
    Write {
        function: FunctionNumber,
        address: IoAddress,
        address_mode: AddressMode,
        transfer_mode: TransferMode,
        bytes: Vec<u8>,
    },
    SetClockHz(u32),
}

/// Correlated SDIO operation emitted by [`crate::AicDevice`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SdioRequest {
    pub id: u64,
    pub kind: SdioRequestKind,
}

/// Successful result shape for one SDIO operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SdioResponse {
    Unit,
    Byte(u8),
    Data(Vec<u8>),
}

/// Correlated SDIO completion returned by the capability adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SdioCompletion {
    pub request_id: u64,
    pub result: Result<SdioResponse, SdioFailure>,
}

/// Bounded snapshot published by the controller hard-interrupt endpoint.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IrqSnapshot {
    pub sequence: u64,
    pub card_interrupt: bool,
    pub transfer_complete: bool,
    pub error: Option<SdioFailure>,
}

/// Owned high-level operation submitted to the device owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlRequest {
    Scan {
        ssid: Option<Vec<u8>>,
    },
    Connect {
        ssid: Vec<u8>,
        pmk: Option<Pmk>,
        entropy: Option<Entropy>,
    },
    Disconnect,
    StartOpenAccessPoint {
        ssid: Vec<u8>,
        channel: u8,
    },
    Cancel,
    Shutdown,
}

/// One explicit event delivered with an owner-provided timestamp.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AicInputEvent {
    Sdio(SdioCompletion),
    Irq(IrqSnapshot),
    Control(ControlRequest),
    Tx { token: TxToken, frame: Vec<u8> },
}

/// Input to one finite advancement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AicInput {
    pub now: MonotonicTime,
    pub event: Option<AicInputEvent>,
}

impl AicInput {
    pub const fn tick(now: MonotonicTime) -> Self {
        Self { now, event: None }
    }
}

/// Externally observable device state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AicState {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxWaitPhase {
    Flow,
    Write,
    Confirmation,
    Complete,
}

/// Non-sensitive identity of the firmware request active at a mailbox timeout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MailboxRequest {
    DebugMemoryRead { address: u32 },
    DebugMemoryWrite { address: u32 },
    DebugMemoryBlockWrite { address: u32, length: u32 },
    DebugMemoryMaskWrite { address: u32 },
    StartApplication { address: u32 },
    Debug { message_id: u16 },
    Lmac { message_id: u16 },
}

/// Completion or data event emitted by the pure core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AicEvent {
    Started { mac_address: [u8; 6] },
    ControlComplete,
    ControlCancelled,
    ControlFailed(AicError),
    Receive(Vec<u8>),
    TransmitComplete(TxToken),
    Stopped,
    Failed(AicError),
}

/// Next action required from the single owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AicAction {
    SubmitSdio(SdioRequest),
    AbortSdio { request_id: u64 },
    RetryAt(MonotonicTime),
    WaitForInterrupt,
    WaitForInterruptUntil(MonotonicTime),
    Event(AicEvent),
    Idle,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AicError {
    #[error("unsupported AIC chip variant")]
    UnsupportedChip,
    #[error("caller did not provide WPA entropy")]
    EntropyUnavailable,
    #[error("invalid control request")]
    InvalidControlRequest,
    #[error("an operation is already active")]
    Busy,
    #[error("SDIO request failed: {0:?}")]
    Sdio(SdioFailure),
    #[error("SDIO completion did not match the active request")]
    CompletionMismatch,
    #[error(
        "AIC mailbox timed out for {request:?} waiting for message {expected_message_id:#06x} in \
         {phase:?} phase"
    )]
    MailboxTimeout {
        request: MailboxRequest,
        expected_message_id: u16,
        phase: MailboxWaitPhase,
    },
    #[error("AIC mailbox response was malformed")]
    MalformedResponse,
    #[error(
        "AIC receive frame at offset {offset} has type {packet_type:#04x}, declared length \
         {declared_length}, and only {available_length} bytes available (header \
         {header_words:016x?})"
    )]
    MalformedRxFrame {
        offset: usize,
        packet_type: u8,
        declared_length: usize,
        available_length: usize,
        header_words: [u64; 3],
    },
    #[error("AIC byte-mode receive length has invalid raw value {units}")]
    InvalidRxByteLength { units: u8 },
    #[error(
        "AIC mailbox response for {request:?} message {expected_message_id:#06x} was malformed \
         ({payload_length} payload bytes)"
    )]
    MalformedMailboxResponse {
        request: MailboxRequest,
        expected_message_id: u16,
        payload_length: usize,
    },
    #[error(
        "AIC mailbox received confirmation {actual_message_id:#06x} while waiting for \
         {expected_message_id:#06x}"
    )]
    UnexpectedConfirmation {
        expected_message_id: u16,
        actual_message_id: u16,
    },
    #[error("AIC CMD52 RAW readback mismatch: expected {expected:#04x}, received {actual:#04x}")]
    SdioWriteReadbackMismatch { expected: u8, actual: u8 },
    #[error("firmware rejected message {message_id:#06x} with status {status}")]
    FirmwareRejected { message_id: u16, status: u16 },
    #[error("firmware rejected debug message {message_id:#06x} with status {status:#010x}")]
    DebugFirmwareRejected { message_id: u16, status: u32 },
    #[error("pinned AIC firmware asset has an invalid internal layout")]
    InvalidFirmwareAsset,
    #[error("firmware returned an invalid MAC address")]
    InvalidMacAddress,
    #[error("WPA2 EAPOL MIC verification failed")]
    WpaMic,
    #[error("WPA2 EAPOL replay validation failed")]
    WpaReplay,
    #[error("WPA2 RSN information does not match the requested profile")]
    WpaRsn,
    #[error("invalid WPA2 handshake message or state")]
    WpaProtocol,
    #[error("WPA2 key data validation failed")]
    WpaKeyData,
    #[error("firmware disconnected the station with reason code {reason_code}")]
    Disconnected { reason_code: u16 },
    #[error("unsupported chip revision {0}")]
    UnsupportedRevision(u8),
    #[error("TX queue is full")]
    TxQueueFull,
    #[error("owner supplied a non-monotonic timestamp")]
    NonMonotonicTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IoPurpose {
    Startup,
    MailboxFlow,
    MailboxWrite,
    ReceiveCount(RxPath),
    ReceiveByteLength(RxPath),
    ReceiveData(RxPath),
    TransmitFlow,
    TransmitData,
    Shutdown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RxPath {
    Command,
    Data,
}

pub(super) struct PendingIo {
    pub id: u64,
    pub purpose: IoPurpose,
}
