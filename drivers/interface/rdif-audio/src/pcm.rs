//! Single-owner PCM playback and capture capability.
//!
//! Unlike [`crate::Playback`], a PCM stream separates explicit stream control
//! from data submission. The card owns every transport and queue and exposes
//! only bounded, typed operations to a task-context worker.

use dma_api::{CpuDmaBuffer, DmaDirection};

use crate::{DriverGeneric, SampleFormat};

/// Smallest period accepted by this first PCM profile.
pub const MIN_PERIOD_FRAMES: u32 = 480;
/// Largest period accepted by this first PCM profile.
pub const MAX_PERIOD_FRAMES: u32 = 960;
/// Frame-count step for period negotiation. One admits any whole frame count.
pub const PERIOD_FRAME_ALIGNMENT: u32 = 1;
/// Smallest advertised PCM period count.
pub const MIN_PERIOD_COUNT: u16 = 4;
/// Largest advertised PCM period count, also bounded by queue depth.
pub const MAX_PERIOD_COUNT: u16 = 8;

/// Direction of an interleaved PCM stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcmDirection {
    Playback,
    Capture,
}

/// A complete PCM configuration accepted before any device state is changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmConfig {
    pub sample_format: SampleFormat,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub period_frames: u32,
    pub period_count: u16,
}

impl PcmConfig {
    /// Bytes in one interleaved frame, or `None` for an invalid channel count.
    pub const fn frame_bytes(self) -> Option<usize> {
        if self.channels == 0 {
            return None;
        }
        let sample_bytes = match self.sample_format {
            SampleFormat::S16Le => 2usize,
        };
        sample_bytes.checked_mul(self.channels as usize)
    }

    /// Bytes in one period, or `None` when the configuration cannot be sized.
    pub const fn period_bytes(self) -> Option<usize> {
        if self.period_frames == 0 {
            return None;
        }
        match self.frame_bytes() {
            Some(frame_bytes) => frame_bytes.checked_mul(self.period_frames as usize),
            None => None,
        }
    }

    /// Bytes covered by the device ring, or `None` on overflow.
    pub const fn buffer_bytes(self) -> Option<usize> {
        if self.period_count == 0 {
            return None;
        }
        match self.period_bytes() {
            Some(period_bytes) => period_bytes.checked_mul(self.period_count as usize),
            None => None,
        }
    }

    /// Whether this configuration belongs to the first supported PCM profile.
    pub fn is_first_profile(self) -> bool {
        matches!(self.sample_format, SampleFormat::S16Le)
            && self.sample_rate_hz == 48_000
            && self.channels == 2
            && (MIN_PERIOD_FRAMES..=MAX_PERIOD_FRAMES).contains(&self.period_frames)
            && (MIN_PERIOD_COUNT..=MAX_PERIOD_COUNT).contains(&self.period_count)
    }
}

/// One PCM direction discovered from the device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmStreamCapabilities {
    /// Device stream identifier; callers must not assume it is zero or one.
    pub stream_id: u32,
    pub direction: PcmDirection,
    pub sample_format: SampleFormat,
    pub sample_rate_hz: u32,
    pub channels_min: u16,
    pub channels_max: u16,
    pub period_frames_min: u32,
    pub period_frames_max: u32,
    pub period_count_min: u16,
    pub period_count_max: u16,
}

/// Capabilities limited to directions the device actually advertised.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PcmCapabilities {
    pub playback: Option<PcmStreamCapabilities>,
    pub capture: Option<PcmStreamCapabilities>,
}

/// Lifecycle state of the card object.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PcmCardState {
    /// The card is waiting for its asynchronous `PCM_INFO` response.
    #[default]
    Discovering,
    /// Device capabilities are available and streams can be opened.
    Ready,
    /// A device reset was requested and status zero has not been confirmed.
    Resetting,
    /// An asynchronous shutdown was requested.
    ShuttingDown,
    /// The device confirmed status zero or a completed shutdown.
    Stopped,
    /// The card is unusable until a confirmed reset establishes a safe state.
    Failed,
}

/// An opened stream generation token.
///
/// The token names a session, not DMA memory. A card rejects stale tokens after
/// release or reopen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmStream {
    stream_id: u32,
    direction: PcmDirection,
    generation: u32,
}

impl PcmStream {
    /// Creates a token for a card implementation after it validates the session.
    pub const fn new(stream_id: u32, direction: PcmDirection, generation: u32) -> Self {
        Self {
            stream_id,
            direction,
            generation,
        }
    }

    pub const fn stream_id(self) -> u32 {
        self.stream_id
    }

    pub const fn direction(self) -> PcmDirection {
        self.direction
    }

    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// A VirtIO PCM control operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcmControlOp {
    Configure,
    Prepare,
    Start,
    Stop,
    Release,
}

/// A control request awaiting a response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmControlToken {
    stream: PcmStream,
    operation: PcmControlOp,
    sequence: u32,
}

impl PcmControlToken {
    pub const fn new(stream: PcmStream, operation: PcmControlOp, sequence: u32) -> Self {
        Self {
            stream,
            operation,
            sequence,
        }
    }

    pub const fn stream(self) -> PcmStream {
        self.stream
    }

    pub const fn operation(self) -> PcmControlOp {
        self.operation
    }

    pub const fn sequence(self) -> u32 {
        self.sequence
    }
}

/// A playback or capture request awaiting a used-ring entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmIoToken {
    stream: PcmStream,
    sequence: u32,
}

impl PcmIoToken {
    pub const fn new(stream: PcmStream, sequence: u32) -> Self {
        Self { stream, sequence }
    }

    pub const fn stream(self) -> PcmStream {
        self.stream
    }

    pub const fn direction(self) -> PcmDirection {
        self.stream.direction
    }

    pub const fn sequence(self) -> u32 {
        self.sequence
    }
}

/// Device event carried by the event queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PcmEventKind {
    JackConnected,
    JackDisconnected,
    PeriodElapsed,
    Xrun,
    Unknown(u32),
}

/// A parsed event. `data` is the stream or jack identifier from the device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmEvent {
    pub kind: PcmEventKind,
    pub data: u32,
}

/// Identity of the card that created a PCM buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmBufferOwner(u32);

impl PcmBufferOwner {
    /// Creates an owner capability for a card implementation.
    ///
    /// # Safety
    ///
    /// `raw` must be an identity issued for the card that allocated every DMA
    /// backing later wrapped with this owner. The issuing card must never reuse
    /// the identity while any buffer carrying it can still exist, and all such
    /// buffers must belong to the same `DeviceDma` domain.
    pub const unsafe fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Raw identity used by the issuing card to validate returned buffers.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// DMA-backed PCM data that is not currently reachable by the device.
///
/// The buffer is movable but not cloneable. A capture completion returns one
/// of these after the used-ring entry and device status have been validated.
pub struct PcmBuffer {
    buffer: CpuDmaBuffer,
    owner: PcmBufferOwner,
    direction: PcmDirection,
    filled_len: usize,
}

impl core::fmt::Debug for PcmBuffer {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PcmBuffer")
            .field("owner", &self.owner)
            .field("direction", &self.direction)
            .field("capacity", &self.len())
            .field("filled_len", &self.filled_len)
            .finish()
    }
}

impl PcmBuffer {
    /// Wraps an owned DMA allocation for a card implementation.
    ///
    /// # Safety
    ///
    /// The device must no longer access `buffer`. `owner` must be the
    /// un-reused capability issued by the same card and DMA domain that
    /// allocated `buffer`. `filled_len` must fit the allocation. This is
    /// normally called only after the exact used-ring completion and device
    /// status have been observed. The direction and size checks performed by
    /// this constructor are diagnostics, not substitutes for these caller
    /// obligations.
    pub unsafe fn from_dma(
        buffer: CpuDmaBuffer,
        owner: PcmBufferOwner,
        direction: PcmDirection,
        filled_len: usize,
    ) -> Result<Self, PcmError> {
        let expected = match direction {
            PcmDirection::Playback => DmaDirection::ToDevice,
            PcmDirection::Capture => DmaDirection::FromDevice,
        };
        if buffer.direction() != expected || filled_len > buffer.len().get() {
            return Err(PcmError::InvalidBuffer);
        }
        Ok(Self {
            buffer,
            owner,
            direction,
            filled_len,
        })
    }

    /// Capacity of the allocation in bytes.
    pub fn len(&self) -> usize {
        self.buffer.len().get()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn direction(&self) -> PcmDirection {
        self.direction
    }

    /// Card identity that created this buffer.
    pub const fn owner(&self) -> PcmBufferOwner {
        self.owner
    }

    pub const fn filled_len(&self) -> usize {
        self.filled_len
    }

    /// Valid PCM bytes in this buffer.
    pub fn as_slice(&self) -> &[u8] {
        &self.buffer.as_slice_cpu()[..self.filled_len]
    }

    /// Replaces the valid contents with a CPU-owned copy.
    pub fn fill_from_slice(&mut self, pcm: &[u8]) -> Result<(), PcmError> {
        if pcm.len() > self.len() {
            return Err(PcmError::InvalidBuffer);
        }
        self.buffer.copy_from_slice_cpu(pcm);
        self.filled_len = pcm.len();
        Ok(())
    }

    /// Copies all valid data into `dst` and returns the copied byte count.
    pub fn copy_to(&self, dst: &mut [u8]) -> Result<usize, PcmError> {
        if dst.len() < self.filled_len {
            return Err(PcmError::InvalidBuffer);
        }
        dst[..self.filled_len].copy_from_slice(self.as_slice());
        Ok(self.filled_len)
    }

    /// Returns the allocation to a card implementation.
    pub fn into_dma(self) -> CpuDmaBuffer {
        self.buffer
    }
}

/// Typed PCM capability errors, independent of OS errno values.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PcmError {
    #[error("PCM configuration or direction is not supported")]
    Unsupported,
    #[error("PCM configuration is invalid")]
    InvalidConfig,
    #[error("PCM buffer is invalid or too small")]
    InvalidBuffer,
    #[error("PCM operation is not valid in the current state")]
    BadState,
    #[error("PCM stream is busy")]
    Busy,
    #[error("PCM queue has no capacity")]
    Again,
    #[error("caller deadline has expired")]
    DeadlineExpired,
    #[error("device response deadline elapsed")]
    Timeout,
    #[error("device protocol response is invalid")]
    Protocol,
    #[error("device or DMA operation failed")]
    Device,
    #[error("card discovery has not completed")]
    NotReady,
    #[error("device stop is not yet confirmed; DMA remains quarantined")]
    ResetPending,
    #[error("public stream or control token identity is exhausted")]
    TokenExhausted,
}

/// Bounded work supplied to one service call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PcmServiceBudget {
    pub control_ops: u8,
    pub playback_ops: u8,
    pub capture_ops: u8,
    pub event_ops: u8,
}

impl PcmServiceBudget {
    pub const fn new(control_ops: u8, playback_ops: u8, capture_ops: u8, event_ops: u8) -> Self {
        Self {
            control_ops,
            playback_ops,
            capture_ops,
            event_ops,
        }
    }

    /// Equal per-queue budget used by a normal task-context worker.
    pub const fn uniform(max_ops: u8) -> Self {
        Self::new(max_ops, max_ops, max_ops, max_ops)
    }
}

impl Default for PcmServiceBudget {
    fn default() -> Self {
        Self::uniform(4)
    }
}

/// Result of one bounded service pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PcmServiceReport {
    pub state: PcmCardState,
    pub state_changed: bool,
    pub control_completed: u8,
    pub playback_completed: u8,
    pub capture_completed: u8,
    pub events: u8,
    pub error: Option<PcmError>,
    /// True only after the transport reported status zero.
    pub transport_stopped: bool,
    /// A queue has progress but the bounded completion queue must be drained.
    pub backpressure: bool,
}

/// A completion or event consumed by the runtime worker.
#[derive(Debug)]
pub enum PcmCompletion {
    /// Initial device discovery finished; capabilities are now available.
    Capabilities,
    Control {
        token: PcmControlToken,
        result: Result<(), PcmError>,
    },
    Playback {
        token: PcmIoToken,
        frames: u32,
    },
    Capture {
        token: PcmIoToken,
        buffer: PcmBuffer,
        frames: u32,
        latency_bytes: u32,
    },
    /// A capture request was cancelled while the stream was closing.
    CaptureCancelled {
        token: PcmIoToken,
    },
    Event(PcmEvent),
    Shutdown {
        confirmed: bool,
    },
    Fault(PcmError),
}

/// A movable, single-worker-owned PCM card.
///
/// All methods take `&mut self` except read-only capability queries. The card
/// owns the transport and all four device queues; callers never receive a queue
/// or device-visible address. `service` is the only operation that advances
/// control, event, playback and capture progress.
pub trait PcmCard: DriverGeneric {
    fn state(&self) -> PcmCardState;

    /// Device capabilities, available after discovery completes.
    fn capabilities(&self) -> Option<&PcmCapabilities>;

    /// Opens the sole session for a direction and assigns a new generation.
    ///
    /// A released session is removed after its `RELEASE` response and all
    /// in-flight DMA for that direction have been reclaimed. The runtime can
    /// then call `open` again; Linux `DROP` therefore maps to stop, release,
    /// reopen, configure and prepare without permanently occupying the slot.
    fn open(&mut self, direction: PcmDirection) -> Result<PcmStream, PcmError>;

    /// Submits `SET_PARAMS` without waiting for the device.
    ///
    /// `now_ns` and `deadline_ns` are absolute monotonic nanoseconds. Expired
    /// or reversed deadlines are rejected before a request is submitted.
    fn configure(
        &mut self,
        stream: PcmStream,
        config: PcmConfig,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError>;

    /// Submits `PREPARE` without waiting for the device.
    fn prepare(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError>;

    /// Submits an explicit `START`; data submission never starts a stream.
    fn start(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError>;

    /// Submits `STOP`. Completion does not release session DMA.
    fn stop(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError>;

    /// Submits `RELEASE`; outstanding TX/RX entries continue to be serviced.
    ///
    /// The session slot closes only after the response is accepted and every
    /// queue entry for this generation has been reclaimed. `RELEASE` is sent
    /// without first waiting for pre-armed capture buffers to finish.
    fn release(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError>;

    /// Copies exactly one configured period into internal playback DMA.
    fn write_playback(
        &mut self,
        stream: PcmStream,
        pcm: &[u8],
        deadline_ns: u64,
    ) -> Result<PcmIoToken, PcmError>;

    /// Returns a completed capture buffer and, when the stream remains active,
    /// re-arms it for another device write.
    ///
    /// When this re-arms a running stream, a zero `deadline_ns` means that the
    /// request has no I/O deadline, as required for a continuous stream. A
    /// non-zero value is an absolute monotonic deadline; an outstanding request
    /// that passes it while the stream is running triggers the card's typed
    /// timeout/reset path. This does not relax the bounded deadlines of STOP,
    /// RELEASE, control commands, or shutdown.
    ///
    /// Ownership of `buffer` transfers to the card on every call and is
    /// consumed regardless of the result. A buffer accepted for an active or
    /// stopping session remains in the capture pool or the bounded close/reset
    /// path until its device access has been quiesced. A missing or stale stream
    /// may return `Ok` after safely consuming the already-closed allocation.
    /// Invalid owner, direction, size, or session state returns a typed error,
    /// and the caller must not reuse the consumed buffer.
    fn recycle_capture(
        &mut self,
        stream: PcmStream,
        buffer: PcmBuffer,
        deadline_ns: u64,
    ) -> Result<(), PcmError>;

    /// Pops one bounded completion. Callers must continue until `None`.
    fn take_completion(&mut self) -> Option<PcmCompletion>;

    /// Advances every queue by at most the supplied per-queue operation budget.
    ///
    /// This method never waits for a device response. It checks control
    /// deadlines, confirms asynchronous reset/shutdown, and returns typed
    /// progress to the worker.
    fn service(&mut self, now_ns: u64, budget: PcmServiceBudget) -> PcmServiceReport;

    /// Starts an asynchronous shutdown. Completion is observed through
    /// `service` and `PcmCompletion::Shutdown`.
    fn begin_shutdown(&mut self, now_ns: u64, deadline_ns: u64) -> Result<(), PcmError>;
}
