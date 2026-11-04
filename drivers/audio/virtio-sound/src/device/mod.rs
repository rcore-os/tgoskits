//! VirtIO Sound transport state machine.
//!
//! Queue buffers, control transactions, PCM stream data, session policy and
//! device lifecycle are kept in focused child modules. The card and transport
//! ownership invariants remain here.

use core::{
    array,
    mem::ManuallyDrop,
    num::NonZeroUsize,
    sync::atomic::{AtomicU32, Ordering},
};

use dma_api::{CpuDmaBuffer, DeviceDma, DmaDirection};
use rdif_audio::{
    DriverGeneric, PcmCapabilities, PcmCardState, PcmCompletion, PcmConfig, PcmDirection, PcmError,
    PcmStream, PcmStreamCapabilities,
};
use virtio_drivers::{
    Hal,
    device::common::Feature,
    queue::VirtQueue,
    transport::{DeviceStatus, DeviceType, Transport},
};

use crate::{
    Error,
    wire::{
        CONTROL_QUEUE, CONTROL_REQUEST_SIZE, CONTROL_RESPONSE_SIZE, EVENT_BUFFER_COUNT,
        EVENT_QUEUE, EVENT_SIZE, MAX_STREAMS, QUEUE_SIZE, RX_QUEUE, STREAMS_CONFIG_OFFSET,
        TX_QUEUE, query_pcm_info_request,
    },
};

mod buffers;
mod control;
mod lifecycle;
mod session;
mod stream;

use buffers::{EventInFlight, RxInFlight, STATUS_BUFFER_ALIGN, TxInFlight};
use control::{ControlInFlight, ControlKind};
use session::{DeferredRelease, StreamSession};

pub(super) const COMPLETION_CAPACITY: usize = 32;
static NEXT_CARD_ID: AtomicU32 = AtomicU32::new(1);

fn next_card_id() -> Option<u32> {
    // Relaxed ordering is sufficient: this value is a uniqueness tag, not a
    // publication fence for any other state.
    NEXT_CARD_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .ok()
}

struct InitGuard<'a, T: Transport> {
    transport: &'a mut T,
    failed: bool,
}

impl<T: Transport> Drop for InitGuard<'_, T> {
    fn drop(&mut self) {
        if self.failed {
            self.transport.set_status(DeviceStatus::empty());
        }
    }
}

/// A movable VirtIO Sound PCM card.
///
/// `H` supplies VirtIO queue DMA. `dma` supplies PCM buffer DMA. The adapter
/// must ensure both describe the same device address domain; this core never
/// translates kernel virtual addresses itself.
///
/// A card in `Resetting` or `ShuttingDown` is the bounded quarantine owner for
/// every queue and in-flight DMA allocation. The runtime must retain it and
/// call `service` until status zero is confirmed. Dropping before that
/// confirmation intentionally leaks this one card's hardware-visible DMA
/// instead of freeing memory the device could still access.
pub struct VirtioSound<H: Hal, T: Transport> {
    id: u32,
    transport: ManuallyDrop<T>,
    dma: DeviceDma,
    control: Option<ManuallyDrop<VirtQueue<H, QUEUE_SIZE>>>,
    event: Option<ManuallyDrop<VirtQueue<H, QUEUE_SIZE>>>,
    tx: Option<ManuallyDrop<VirtQueue<H, QUEUE_SIZE>>>,
    rx: Option<ManuallyDrop<VirtQueue<H, QUEUE_SIZE>>>,
    queues_dropped: bool,
    state: PcmCardState,
    state_changed: bool,
    discovered: bool,
    capabilities: PcmCapabilities,
    next_generation: u32,
    next_control_sequence: u32,
    next_io_sequence: u32,
    deferred_release: [Option<DeferredRelease>; 2],
    playback: Option<StreamSession>,
    capture: Option<StreamSession>,
    control_pending: Option<ControlInFlight>,
    control_request_pool: Option<CpuDmaBuffer>,
    control_response_pool: Option<CpuDmaBuffer>,
    event_pool: [Option<CpuDmaBuffer>; EVENT_BUFFER_COUNT],
    event_inflight: [Option<EventInFlight>; EVENT_BUFFER_COUNT],
    tx_inflight: [Option<TxInFlight>; buffers::MAX_PERIODS],
    rx_inflight: [Option<RxInFlight>; buffers::MAX_PERIODS],
    completions: [Option<PcmCompletion>; COMPLETION_CAPACITY],
    completion_head: usize,
    completion_len: usize,
    shutdown_deadline_ns: Option<u64>,
    shutdown_signaled: bool,
    fault_signaled: bool,
    error: Option<PcmError>,
    transport_stopped: bool,
}

impl<H: Hal, T: Transport> VirtioSound<H, T> {
    /// Constructs a card and submits an asynchronous `PCM_INFO` request.
    ///
    /// Construction never polls for the response. Call [`rdif_audio::PcmCard::service`]
    /// afterwards; capabilities become available when discovery completes or
    /// the discovery deadline expires.
    ///
    /// Failures while preparing fallible buffers return `Err` before any queue
    /// is registered. A failure while initializing queues or submitting initial
    /// work returns a card in [`PcmCardState::Resetting`], so every possible
    /// queue DMA owner remains bounded and reachable by `service` until status
    /// zero is confirmed.
    ///
    /// # Safety
    ///
    /// `transport` must exclusively address this VirtIO Sound device and must
    /// remain owned by the returned card until a confirmed status-zero reset or
    /// card destruction. `H::dma_alloc`, `H::share` and `H::unshare` must map
    /// memory in the same device address domain as `dma`, with the coherency
    /// and direction rules of both capabilities. In particular, every pointer
    /// returned by `dma` and passed through `VirtQueue` must be device-readable
    /// or device-writable exactly as requested. The caller must also ensure no
    /// other driver resets or configures the transport concurrently.
    pub unsafe fn new(
        mut transport: T,
        dma: DeviceDma,
        now_ns: u64,
        discovery_deadline_ns: u64,
    ) -> Result<Self, Error> {
        if transport.device_type() != DeviceType::Sound {
            return Err(Error::NotSoundDevice);
        }
        if discovery_deadline_ns == 0 || discovery_deadline_ns <= now_ns {
            return Err(Error::DeadlineExpired);
        }
        let card_id = next_card_id().ok_or(Error::CardIdExhausted)?;

        let mut guard = InitGuard {
            transport: &mut transport,
            failed: true,
        };
        let features = guard.transport.begin_init(Feature::VERSION_1);
        if !guard.transport.requires_legacy_layout() {
            let status = guard.transport.get_status();
            if !features.contains(Feature::VERSION_1) || !status.contains(DeviceStatus::FEATURES_OK)
            {
                guard.transport.set_status(status | DeviceStatus::FAILED);
                return Err(Error::Unsupported);
            }
        }

        let transport_ref = &*guard.transport;
        let stream_count: u32 = transport_ref
            .read_consistent(|| transport_ref.read_config_space(STREAMS_CONFIG_OFFSET))?;
        if stream_count == 0 || stream_count > MAX_STREAMS {
            return Err(Error::Unsupported);
        }

        let control_request_pool = Some(CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(CONTROL_REQUEST_SIZE).ok_or(Error::InvalidConfig)?,
            STATUS_BUFFER_ALIGN,
            DmaDirection::ToDevice,
        )?);
        let control_response_pool = Some(CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(CONTROL_RESPONSE_SIZE).ok_or(Error::InvalidConfig)?,
            STATUS_BUFFER_ALIGN,
            DmaDirection::FromDevice,
        )?);
        let event_len = NonZeroUsize::new(EVENT_SIZE).ok_or(Error::InvalidConfig)?;
        let mut event_pool: [Option<CpuDmaBuffer>; EVENT_BUFFER_COUNT] = array::from_fn(|_| None);
        for slot in &mut event_pool {
            *slot = Some(CpuDmaBuffer::new_zero(
                &dma,
                event_len,
                STATUS_BUFFER_ALIGN,
                DmaDirection::FromDevice,
            )?);
        }

        guard.failed = false;
        drop(guard);

        let mut card = Self {
            id: card_id,
            transport: ManuallyDrop::new(transport),
            dma,
            control: None,
            event: None,
            tx: None,
            rx: None,
            queues_dropped: false,
            state: PcmCardState::Discovering,
            state_changed: false,
            discovered: false,
            capabilities: PcmCapabilities::default(),
            next_generation: 1,
            next_control_sequence: 1,
            next_io_sequence: 1,
            deferred_release: [None, None],
            playback: None,
            capture: None,
            control_pending: None,
            control_request_pool,
            control_response_pool,
            event_pool,
            event_inflight: array::from_fn(|_| None),
            tx_inflight: array::from_fn(|_| None),
            rx_inflight: array::from_fn(|_| None),
            completions: array::from_fn(|_| None),
            completion_head: 0,
            completion_len: 0,
            shutdown_deadline_ns: None,
            shutdown_signaled: false,
            fault_signaled: false,
            error: None,
            transport_stopped: false,
        };

        if let Err(error) = card.initialize_queues(stream_count, discovery_deadline_ns) {
            card.request_reset(error.as_pcm());
        }
        Ok(card)
    }

    fn initialize_queues(
        &mut self,
        stream_count: u32,
        discovery_deadline_ns: u64,
    ) -> Result<(), Error> {
        self.control = Some(ManuallyDrop::new(VirtQueue::new(
            &mut *self.transport,
            CONTROL_QUEUE,
            false,
            false,
        )?));
        self.event = Some(ManuallyDrop::new(VirtQueue::new(
            &mut *self.transport,
            EVENT_QUEUE,
            false,
            false,
        )?));
        self.tx = Some(ManuallyDrop::new(VirtQueue::new(
            &mut *self.transport,
            TX_QUEUE,
            false,
            false,
        )?));
        self.rx = Some(ManuallyDrop::new(VirtQueue::new(
            &mut *self.transport,
            RX_QUEUE,
            false,
            false,
        )?));
        self.set_used_notifications(false);
        self.transport.finish_init();
        self.refill_events(EVENT_BUFFER_COUNT)?;
        let request = query_pcm_info_request(stream_count);
        self.submit_control(
            ControlKind::Discovery { stream_count },
            None,
            discovery_deadline_ns,
            &request,
        )?;
        Ok(())
    }

    /// Enables or suppresses device-to-driver used-buffer notifications.
    ///
    /// The worker notifications sent by the driver after publishing available
    /// descriptors are always retained. Polling service does not require used
    /// notifications, so construction leaves them disabled.
    pub fn set_used_notifications(&mut self, enabled: bool) {
        if self.queues_dropped {
            return;
        }
        if let Some(control) = &mut self.control {
            control.set_dev_notify(enabled);
        }
        if let Some(event) = &mut self.event {
            event.set_dev_notify(enabled);
        }
        if let Some(tx) = &mut self.tx {
            tx.set_dev_notify(enabled);
        }
        if let Some(rx) = &mut self.rx {
            rx.set_dev_notify(enabled);
        }
    }

    fn session(&self, stream: PcmStream) -> Option<&StreamSession> {
        let session = match stream.direction() {
            PcmDirection::Playback => self.playback.as_ref(),
            PcmDirection::Capture => self.capture.as_ref(),
        }?;
        (session.stream == stream).then_some(session)
    }

    fn session_mut(&mut self, stream: PcmStream) -> Result<&mut StreamSession, PcmError> {
        let session = match stream.direction() {
            PcmDirection::Playback => self.playback.as_mut(),
            PcmDirection::Capture => self.capture.as_mut(),
        }
        .ok_or(PcmError::BadState)?;
        if session.stream != stream {
            return Err(PcmError::BadState);
        }
        Ok(session)
    }

    fn supports(&self, direction: PcmDirection) -> Option<PcmStreamCapabilities> {
        match direction {
            PcmDirection::Playback => self.capabilities.playback,
            PcmDirection::Capture => self.capabilities.capture,
        }
    }

    fn supported_config(&self, direction: PcmDirection, config: PcmConfig) -> bool {
        if !config.is_first_profile() {
            return false;
        }
        let Some(caps) = self.supports(direction) else {
            return false;
        };
        config.sample_format == caps.sample_format
            && config.sample_rate_hz == caps.sample_rate_hz
            && (caps.channels_min..=caps.channels_max).contains(&config.channels)
            && (caps.period_frames_min..=caps.period_frames_max).contains(&config.period_frames)
            && (caps.period_count_min..=caps.period_count_max).contains(&config.period_count)
    }

    fn push_completion(&mut self, completion: PcmCompletion) -> bool {
        if self.completion_len >= COMPLETION_CAPACITY {
            return false;
        }
        let index = (self.completion_head + self.completion_len) % COMPLETION_CAPACITY;
        self.completions[index] = Some(completion);
        self.completion_len += 1;
        true
    }

    fn has_completion_capacity(&self) -> bool {
        self.completion_len < COMPLETION_CAPACITY
    }

    fn completion_capacity(&self) -> usize {
        COMPLETION_CAPACITY - self.completion_len
    }
}

impl<H: Hal + 'static, T: Transport + Send + 'static> DriverGeneric for VirtioSound<H, T> {
    fn name(&self) -> &str {
        "VirtIO Sound PCM"
    }
}
