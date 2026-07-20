use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::Cell,
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use rdif_irq::{
    ContainmentCause, FaultContainment, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource,
};

use crate::{
    Config, ConfigError, InterruptMask, IrqSource, RawUart, RxFlag, RxItem, SerialActivationError,
    SerialCounters, SerialIrqCapture, SerialIrqEvent, SerialIrqEvents, SerialIrqFault,
    SerialMaskedService, SerialRearmError, SpscRing,
};

pub const DEFAULT_TX_CAP: usize = 4097;
pub const DEFAULT_RX_CAP: usize = 4097;

pub const RX_IRQ_BUDGET: usize = 256;
pub const TX_IRQ_BUDGET: usize = 64;
pub const TX_KICK_BUDGET: usize = 32;
/// Maximum bytes written by one emergency ownership attempt.
pub const EMERGENCY_TX_BUDGET: usize = 64;
/// Maximum transmitter-idle status reads made by one emergency flush.
pub const EMERGENCY_FLUSH_POLL_BUDGET: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxSubmit {
    pub accepted: usize,
    pub needs_kick: bool,
}

/// Result of one bounded, non-blocking emergency UART write attempt.
///
/// This operation never stages bytes in the software TX queue. `Busy` means
/// either the runtime UART register owner or the hardware transmitter cannot
/// currently make progress. `Fault` means the runtime port is not running and
/// therefore cannot be used as the post-handover emergency console.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum EmergencyWriteResult {
    Written { count: usize },
    Busy,
    Fault,
}

/// Result of one bounded emergency transmitter-drain attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[must_use]
pub enum EmergencyFlushResult {
    Flushed,
    Busy,
    Fault,
}

pub struct TxState<const N: usize> {
    ring: SpscRing<u8, N>,
    blocked: AtomicBool,
    submitted: AtomicUsize,
    sent: AtomicUsize,
}

impl<const N: usize> TxState<N> {
    fn new() -> Self {
        Self {
            ring: SpscRing::new(),
            blocked: AtomicBool::new(false),
            submitted: AtomicUsize::new(0),
            sent: AtomicUsize::new(0),
        }
    }

    fn write_room(&self) -> usize {
        self.ring.remaining_snapshot()
    }

    fn chars_in_buffer(&self) -> usize {
        self.ring.len_snapshot()
    }

    fn clear_from_owner(&self) {
        self.ring.clear_consumer();
        self.blocked.store(false, Ordering::Release);
    }
}

pub struct TxQueue<const N: usize = DEFAULT_TX_CAP> {
    state: Arc<TxState<N>>,
    _single_producer: PhantomData<Cell<()>>,
}

unsafe impl<const N: usize> Send for TxQueue<N> {}

impl<const N: usize> TxQueue<N> {
    pub fn submit(&mut self, bytes: &[u8]) -> TxSubmit {
        let mut accepted = 0;
        for &byte in bytes {
            if self.state.ring.push(byte).is_err() {
                self.state.blocked.store(true, Ordering::Release);
                break;
            }
            accepted += 1;
        }
        self.state.submitted.fetch_add(accepted, Ordering::Relaxed);
        TxSubmit {
            accepted,
            needs_kick: accepted > 0,
        }
    }

    pub fn write_room(&self) -> usize {
        self.state.write_room()
    }

    pub fn chars_in_buffer(&self) -> usize {
        self.state.chars_in_buffer()
    }
}

pub struct RxState<const N: usize> {
    ring: SpscRing<RxItem, N>,
    pushed: AtomicUsize,
    dropped: AtomicUsize,
    overrun: AtomicUsize,
    rearm_generation: AtomicUsize,
}

impl<const N: usize> RxState<N> {
    fn new() -> Self {
        Self {
            ring: SpscRing::new(),
            pushed: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            overrun: AtomicUsize::new(0),
            rearm_generation: AtomicUsize::new(0),
        }
    }

    fn push_from_owner(&self, item: RxItem) -> bool {
        match self.ring.push(item) {
            Ok(()) => {
                self.pushed.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    fn clear_from_owner(&self) {
        self.ring.clear_consumer();
    }
}

pub struct RxQueue<const N: usize = DEFAULT_RX_CAP> {
    state: Arc<RxState<N>>,
    _single_consumer: PhantomData<Cell<()>>,
}

unsafe impl<const N: usize> Send for RxQueue<N> {}

impl<const N: usize> RxQueue<N> {
    pub fn drain(&mut self, out: &mut [RxItem]) -> RxDrain {
        let mut count = 0;
        for slot in out {
            let Some(item) = self.state.ring.pop() else {
                break;
            };
            *slot = item;
            count += 1;
        }
        let rearm = if count == 0 {
            None
        } else {
            let generation = self.state.rearm_generation.swap(0, Ordering::AcqRel);
            (generation != 0).then(|| masked_source(generation, InterruptMask::RX))
        };
        RxDrain { count, rearm }
    }

    pub fn rx_pending(&self) -> bool {
        !self.state.ring.is_empty()
    }
}

/// RX drain result that can mint a rearm request only after releasing space.
#[derive(Debug, Eq, PartialEq)]
pub struct RxDrain {
    pub count: usize,
    pub rearm: Option<MaskedSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortState {
    Down,
    Polling,
    Prepared,
    Running,
    Faulted,
}

struct CoreInner<T: RawUart> {
    raw: T,
    irq_mask: InterruptMask,
    state: PortState,
    tx_irq_enabled: bool,
    generation: usize,
    rx_backpressured: bool,
    masked_generation: usize,
    masked_sources: InterruptMask,
}

impl<T: RawUart> CoreInner<T> {
    fn new(raw: T) -> Self {
        Self {
            raw,
            irq_mask: InterruptMask::empty(),
            state: PortState::Down,
            tx_irq_enabled: false,
            generation: 0,
            rx_backpressured: false,
            masked_generation: 0,
            masked_sources: InterruptMask::empty(),
        }
    }
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct SerialSoftWork: u32 {
        const TX_KICK = 1 << 0;
    }
}

type DynRawUart = Box<dyn RawUart>;

pub struct SerialParts<const TX: usize = DEFAULT_TX_CAP, const RX: usize = DEFAULT_RX_CAP> {
    pub core: SerialCore<TX, RX>,
    pub tx: TxQueue<TX>,
    pub rx: RxQueue<RX>,
}

/// Mutable portable UART runtime serialized by the consuming OS glue.
pub struct SerialCore<const TX: usize = DEFAULT_TX_CAP, const RX: usize = DEFAULT_RX_CAP> {
    inner: CoreInner<DynRawUart>,
    tx: Arc<TxState<TX>>,
    rx: Arc<RxState<RX>>,
    counters: Arc<SerialCountersAtomic>,
}

impl<const TX: usize, const RX: usize> SerialCore<TX, RX> {
    pub fn split(raw: impl RawUart) -> SerialParts<TX, RX> {
        Self::split_boxed(Box::new(raw))
    }

    pub fn split_boxed(raw: Box<dyn RawUart>) -> SerialParts<TX, RX> {
        let tx = Arc::new(TxState::new());
        let rx = Arc::new(RxState::new());
        let counters = Arc::new(SerialCountersAtomic::new());
        let core = Self {
            inner: CoreInner::new(raw),
            tx: tx.clone(),
            rx: rx.clone(),
            counters: counters.clone(),
        };
        SerialParts {
            core,
            tx: TxQueue {
                state: tx,
                _single_producer: PhantomData,
            },
            rx: RxQueue {
                state: rx,
                _single_consumer: PhantomData,
            },
        }
    }

    /// Attempts a bounded emergency write in the unique owner domain.
    pub fn try_write_emergency(&mut self, bytes: &[u8]) -> EmergencyWriteResult {
        if bytes.is_empty() {
            return EmergencyWriteResult::Written { count: 0 };
        }
        if self.inner.state != PortState::Running {
            return EmergencyWriteResult::Fault;
        }

        let mut count = 0;
        while count < bytes.len().min(EMERGENCY_TX_BUDGET) && self.inner.raw.tx_ready() {
            self.inner.raw.write_tx(bytes[count]);
            count += 1;
        }
        if count == 0 {
            return EmergencyWriteResult::Busy;
        }
        self.counters.tx_bytes.fetch_add(count, Ordering::Relaxed);
        EmergencyWriteResult::Written { count }
    }

    /// Makes one bounded transmitter-drain attempt in the unique owner domain.
    pub fn try_flush_emergency(&mut self) -> EmergencyFlushResult {
        if self.inner.state != PortState::Running {
            return EmergencyFlushResult::Fault;
        }
        for _ in 0..EMERGENCY_FLUSH_POLL_BUDGET {
            if self.inner.raw.tx_idle() {
                return EmergencyFlushResult::Flushed;
            }
        }
        EmergencyFlushResult::Busy
    }

    /// Prepares the port while keeping every device IRQ source masked.
    ///
    /// The OS must first enable the registered same-CPU IRQ action and then
    /// call [`Self::activate_interrupts`] from the maintenance owner.
    pub fn startup(&mut self, config: &Config) -> Result<(), ConfigError> {
        if matches!(self.inner.state, PortState::Prepared | PortState::Running) {
            return Ok(());
        }

        self.bump_generation();
        if self.inner.state == PortState::Faulted {
            self.inner.raw.shutdown();
            self.inner.state = PortState::Down;
        }

        self.inner.raw.startup(config)?;
        self.inner.irq_mask = InterruptMask::empty();
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        self.inner.tx_irq_enabled = false;
        self.inner.rx_backpressured = false;
        self.clear_masked();
        self.rx.rearm_generation.store(0, Ordering::Release);
        self.inner.state = PortState::Prepared;
        Ok(())
    }

    /// Enables runtime device sources after the OS IRQ action is live.
    ///
    /// The maintenance owner must call this only after registering and
    /// enabling its same-CPU action. This ordering prevents a device IRQ from
    /// becoming observable before a handler owns the endpoint.
    pub fn activate_interrupts(&mut self) -> Result<(), SerialActivationError> {
        if self.inner.state == PortState::Running {
            return Ok(());
        }
        if self.inner.state != PortState::Prepared {
            return Err(SerialActivationError::NotPrepared);
        }
        self.inner.irq_mask = InterruptMask::RX;
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        self.inner.state = PortState::Running;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        self.bump_generation();
        self.clear_masked();
        self.rx.rearm_generation.store(0, Ordering::Release);
        if self.inner.state == PortState::Down {
            return;
        }

        self.inner.raw.set_irq_mask(InterruptMask::empty());
        self.inner.raw.shutdown();
        self.inner.irq_mask = InterruptMask::empty();
        self.inner.tx_irq_enabled = false;
        self.inner.rx_backpressured = false;
        self.inner.state = PortState::Down;
        self.tx.clear_from_owner();
        self.rx.clear_from_owner();
    }

    /// Quiesces the interrupt-driven core without disabling the UART.
    ///
    /// This transition is intended for an early-console handover whose OS IRQ
    /// registration could not be enabled after [`Self::startup`] armed the
    /// device. It masks every device interrupt and returns the portable runtime
    /// core to polling state, while deliberately preserving the UART enable
    /// state, line divisor, and other polling configuration owned by the boot
    /// console.
    /// Software queues are discarded because no runtime consumer was published.
    pub fn quiesce_to_polling(&mut self) {
        self.bump_generation();
        self.inner.raw.set_irq_mask(InterruptMask::empty());
        self.inner.irq_mask = InterruptMask::empty();
        self.inner.tx_irq_enabled = false;
        self.inner.rx_backpressured = false;
        self.clear_masked();
        self.inner.state = PortState::Polling;
        self.rx.rearm_generation.store(0, Ordering::Release);
        self.tx.clear_from_owner();
        self.rx.clear_from_owner();
    }

    pub fn set_config(&mut self, config: &Config) -> Result<(), ConfigError> {
        let saved_mask = self.inner.irq_mask;
        self.inner.raw.set_irq_mask(InterruptMask::empty());
        let result = self.inner.raw.set_config(config);
        self.inner.raw.set_irq_mask(saved_mask);
        result
    }

    pub fn baudrate(&self) -> u32 {
        self.inner.raw.baudrate()
    }

    pub fn tx_idle(&mut self) -> bool {
        self.tx.ring.is_empty() && self.inner.raw.tx_idle()
    }

    pub fn counters(&self) -> SerialCounters {
        self.counters.snapshot()
    }

    /// Acknowledges one hardware IRQ snapshot and masks serviceable sources.
    ///
    /// This hard-IRQ endpoint never drains RX, fills TX, completes requests, or
    /// rearms a source. The unique maintenance owner consumes the returned
    /// masked-source token through [`Self::service_masked`]. Losing the event
    /// therefore leaves the precise device source safely masked.
    pub fn capture_irq(&mut self) -> SerialIrqCapture {
        if self.inner.state != PortState::Running {
            return SerialIrqCapture::Unhandled;
        }

        let snapshot = self.inner.raw.take_irq_snapshot();
        if !snapshot.claimed {
            self.counters.irq_spurious.fetch_add(1, Ordering::Relaxed);
            return SerialIrqCapture::Unhandled;
        }
        self.counters.irq_total.fetch_add(1, Ordering::Relaxed);
        let sources = snapshot.sources;
        if sources.is_empty() || sources.contains(IrqSource::OTHER_ACK) {
            return self.fault_capture(SerialIrqFault::UnknownSource);
        }
        if sources.contains(IrqSource::BUSY_DETECT) {
            self.inner.raw.ack_busy_detect();
            return self.fault_capture(SerialIrqFault::UnmaskableSource);
        }
        if sources.contains(IrqSource::MODEM_STATUS) {
            self.inner.raw.ack_modem_status();
        }

        let Some(source_mask) = interrupt_mask_for_sources(sources) else {
            return SerialIrqCapture::Captured {
                event: SerialIrqEvent::new(sources),
                masked: None,
            };
        };

        self.mask_observed_sources(sources, source_mask)
    }

    /// Performs one bounded owner-side pass for a captured masked source set.
    pub fn service_masked(&mut self, source: MaskedSource) -> SerialMaskedService {
        let Ok(source_mask) = self.validate_masked_source(source) else {
            return SerialMaskedService::Stale;
        };
        if !self.inner.masked_sources.contains(source_mask)
            || self.inner.state != PortState::Running
        {
            return SerialMaskedService::Stale;
        }

        let mut events = SerialIrqEvents::default();
        let mut pending = false;
        if source_mask.intersects(InterruptMask::RX) {
            if self.inner.rx_backpressured {
                return SerialMaskedService::Backpressured(events);
            }
            let service = self.service_rx(RX_IRQ_BUDGET);
            events.rx_pushed = service.published;
            if service.backpressured {
                return SerialMaskedService::Backpressured(events);
            }
            pending |= service.consumed == RX_IRQ_BUDGET;
        }
        if source_mask.contains(InterruptMask::TX_SPACE) {
            let sent = match self.service_tx(TX_IRQ_BUDGET, &mut events, TxMaskUpdate::Masked) {
                Ok(sent) => sent,
                Err(reason) => return self.fault_masked_service(events, reason),
            };
            pending |= sent == TX_IRQ_BUDGET && !self.tx.ring.is_empty();
        }
        if pending {
            self.counters
                .service_budget_exhausted
                .fetch_add(1, Ordering::Relaxed);
            SerialMaskedService::Pending(events)
        } else {
            SerialMaskedService::Complete(events)
        }
    }

    /// Explicitly rearms a source set completed by owner-side service.
    pub fn rearm_masked(
        &mut self,
        source: MaskedSource,
    ) -> Result<InterruptMask, SerialRearmError> {
        let sources = self.validate_masked_source(source)?;
        if sources.intersects(InterruptMask::RX) && self.inner.rx_backpressured {
            if self.rx.ring.remaining_snapshot() < 2 {
                return Err(SerialRearmError::RxBackpressured);
            }
            self.inner.rx_backpressured = false;
            self.rx.rearm_generation.store(0, Ordering::Release);
        }

        let mut enabled = sources;
        if sources.contains(InterruptMask::TX_SPACE) {
            if self.tx.ring.is_empty() {
                enabled.remove(InterruptMask::TX_SPACE);
                self.inner.tx_irq_enabled = false;
            } else {
                self.inner.tx_irq_enabled = true;
            }
        }
        self.inner.masked_sources.remove(sources);
        if self.inner.masked_sources.is_empty() {
            self.inner.masked_generation = 0;
        }
        self.inner.irq_mask.insert(enabled);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        Ok(enabled)
    }

    /// Performs bounded software-originated work without probing IRQ status.
    pub fn service(&mut self, work: SerialSoftWork) -> Result<SerialIrqEvents, SerialIrqFault> {
        let mut events = SerialIrqEvents::default();
        if self.inner.state == PortState::Running
            && work.contains(SerialSoftWork::TX_KICK)
            && let Err(reason) = self.service_tx(TX_KICK_BUDGET, &mut events, TxMaskUpdate::Manage)
        {
            self.quarantine(reason);
            return Err(reason);
        }
        Ok(events)
    }

    fn bump_generation(&mut self) {
        self.inner.generation = self
            .inner
            .generation
            .checked_add(1)
            .expect("serial generation exhausted");
    }

    fn mask_observed_sources(
        &mut self,
        sources: IrqSource,
        source_mask: InterruptMask,
    ) -> SerialIrqCapture {
        let source_mask = source_mask & self.inner.irq_mask;
        if source_mask.is_empty() {
            return self.fault_capture(SerialIrqFault::UnmaskableSource);
        }
        self.inner.irq_mask.remove(source_mask);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        self.record_masked_sources(source_mask);
        SerialIrqCapture::Captured {
            event: SerialIrqEvent::new(sources),
            masked: Some(masked_source(self.inner.generation, source_mask)),
        }
    }

    fn record_masked_sources(&mut self, sources: InterruptMask) {
        self.inner.masked_generation = self.inner.generation;
        self.inner.masked_sources.insert(sources);
    }

    fn clear_masked_sources(&mut self, sources: InterruptMask) {
        self.inner.masked_sources.remove(sources);
        if self.inner.masked_sources.is_empty() {
            self.inner.masked_generation = 0;
        }
    }

    fn clear_masked(&mut self) {
        self.inner.masked_generation = 0;
        self.inner.masked_sources = InterruptMask::empty();
    }

    fn validate_masked_source(
        &self,
        source: MaskedSource,
    ) -> Result<InterruptMask, SerialRearmError> {
        let generation =
            usize::try_from(source.generation().get()).map_err(|_| SerialRearmError::Stale)?;
        if generation != self.inner.generation
            || generation != self.inner.masked_generation
            || self.inner.state != PortState::Running
        {
            return Err(SerialRearmError::Stale);
        }
        let bitmap =
            u32::try_from(source.bitmap().get()).map_err(|_| SerialRearmError::NotCaptured)?;
        let sources = InterruptMask::from_bits(bitmap).ok_or(SerialRearmError::NotCaptured)?;
        if sources.is_empty() || !self.inner.masked_sources.contains(sources) {
            return Err(SerialRearmError::NotCaptured);
        }
        Ok(sources)
    }

    fn service_rx(&mut self, budget: usize) -> RxService {
        let mut result = RxService::default();
        for _ in 0..budget {
            if self.rx.ring.remaining_snapshot() < 2 {
                result.backpressured = self.mask_rx_for_backpressure();
                break;
            }
            let Some(sample) = self.inner.raw.read_rx() else {
                break;
            };
            result.consumed += 1;
            self.publish_rx_sample(sample, &mut result);
        }
        result
    }

    fn publish_rx_sample(&self, sample: crate::RxSample, result: &mut RxService) {
        match sample.flag {
            RxFlag::Normal => {}
            RxFlag::Break => {
                self.counters.rx_breaks.fetch_add(1, Ordering::Relaxed);
            }
            RxFlag::Parity => {
                self.counters
                    .rx_parity_errors
                    .fetch_add(1, Ordering::Relaxed);
            }
            RxFlag::Framing => {
                self.counters
                    .rx_framing_errors
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        if let Some(byte) = sample.byte {
            self.counters.rx_bytes.fetch_add(1, Ordering::Relaxed);
            if self.rx.push_from_owner(RxItem::Byte {
                byte,
                flag: sample.flag,
            }) {
                result.published += 1;
            }
        }
        if sample.overrun {
            self.rx.overrun.fetch_add(1, Ordering::Relaxed);
            self.counters
                .rx_fifo_overruns
                .fetch_add(1, Ordering::Relaxed);
            if self.rx.push_from_owner(RxItem::Overrun) {
                result.published += 1;
            }
        }
    }

    fn mask_rx_for_backpressure(&mut self) -> bool {
        self.inner.rx_backpressured = true;
        let generation = self.inner.generation;
        self.record_masked_sources(InterruptMask::RX);
        self.rx
            .rearm_generation
            .store(generation, Ordering::Release);
        self.inner.irq_mask.remove(InterruptMask::RX);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        if self.rx.ring.remaining_snapshot() >= 2
            && self
                .rx
                .rearm_generation
                .compare_exchange(generation, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            self.inner.rx_backpressured = false;
            self.clear_masked_sources(InterruptMask::RX);
            self.inner.irq_mask.insert(InterruptMask::RX);
            self.inner.raw.set_irq_mask(self.inner.irq_mask);
            return false;
        }
        true
    }

    fn service_tx(
        &mut self,
        budget: usize,
        events: &mut SerialIrqEvents,
        mask_update: TxMaskUpdate,
    ) -> Result<usize, SerialIrqFault> {
        let load_size = self.inner.raw.tx_load_size();
        if load_size == 0 {
            return Err(SerialIrqFault::InvalidTransmitLoad);
        }
        let limit = budget.min(load_size);
        let mut sent = 0;
        while sent < limit && self.inner.raw.tx_ready() {
            let Some(byte) = self.tx.ring.peek_copy() else {
                break;
            };
            self.inner.raw.write_tx(byte);
            let committed = self.tx.ring.pop();
            debug_assert_eq!(committed, Some(byte));
            self.tx.sent.fetch_add(1, Ordering::Relaxed);
            self.counters.tx_bytes.fetch_add(1, Ordering::Relaxed);
            sent += 1;
        }

        if mask_update == TxMaskUpdate::Manage && self.tx.ring.is_empty() {
            if self.inner.tx_irq_enabled {
                self.inner.irq_mask.remove(InterruptMask::TX_SPACE);
                self.inner.raw.set_irq_mask(self.inner.irq_mask);
                self.inner.tx_irq_enabled = false;
            }
        } else if mask_update == TxMaskUpdate::Manage && !self.inner.tx_irq_enabled {
            self.inner.irq_mask.insert(InterruptMask::TX_SPACE);
            self.inner.raw.set_irq_mask(self.inner.irq_mask);
            self.inner.tx_irq_enabled = true;
        }

        if sent > 0 {
            self.tx.blocked.store(false, Ordering::Release);
            events.tx_wakeup = true;
        }
        events.tx_sent += sent;
        Ok(sent)
    }

    fn fault_capture(&mut self, reason: SerialIrqFault) -> SerialIrqCapture {
        // Mask every known source as a best-effort containment step, but do not
        // claim that an unknown or unmaskable source was precisely isolated.
        let _ = self.contain_sources();
        self.enter_faulted();
        SerialIrqCapture::Fault {
            reason,
            containment: FaultContainment::Uncontained,
        }
    }

    fn fault_masked_service(
        &mut self,
        events: SerialIrqEvents,
        reason: SerialIrqFault,
    ) -> SerialMaskedService {
        self.quarantine(reason);
        let _ = events;
        SerialMaskedService::Fault(reason)
    }

    fn quarantine(&mut self, _fault: SerialIrqFault) {
        let _ = self.contain_sources();
        self.enter_faulted();
    }

    fn contain_sources(&mut self) -> Result<MaskedSource, SerialIrqFault> {
        if !matches!(self.inner.state, PortState::Running | PortState::Faulted) {
            return Err(SerialIrqFault::UnmaskableSource);
        }
        let sources = self.inner.irq_mask | self.inner.masked_sources;
        if sources.is_empty() {
            return Err(SerialIrqFault::UnmaskableSource);
        }
        self.inner.irq_mask.remove(sources);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        self.record_masked_sources(sources);
        Ok(masked_source(self.inner.generation, sources))
    }

    fn enter_faulted(&mut self) {
        self.inner.tx_irq_enabled = false;
        self.inner.rx_backpressured = false;
        self.inner.state = PortState::Faulted;
        self.rx.rearm_generation.store(0, Ordering::Release);
    }
}

impl<const TX: usize, const RX: usize> IrqEndpoint for SerialCore<TX, RX> {
    type Event = SerialIrqEvent;
    type Fault = SerialIrqFault;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        self.capture_irq()
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        self.contain_sources()
    }
}

impl<const TX: usize, const RX: usize> IrqSourceControl for SerialCore<TX, RX> {
    type Error = SerialRearmError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        self.rearm_masked(source).map(|_| ())
    }
}

fn masked_source(generation: usize, sources: InterruptMask) -> MaskedSource {
    let generation = u64::try_from(generation).expect("serial generation exceeds u64");
    MaskedSource::try_new(generation, u64::from(sources.bits()))
        .expect("running serial generations and masked source sets are nonzero")
}

fn interrupt_mask_for_sources(sources: IrqSource) -> Option<InterruptMask> {
    let mut mask = InterruptMask::empty();
    if sources.intersects(IrqSource::RX_DATA | IrqSource::RX_TIMEOUT | IrqSource::RX_STATUS) {
        mask |= InterruptMask::RX;
    }
    if sources.contains(IrqSource::TX_SPACE) {
        mask |= InterruptMask::TX_SPACE;
    }
    (!mask.is_empty()).then_some(mask)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TxMaskUpdate {
    Manage,
    Masked,
}

#[derive(Default)]
struct RxService {
    consumed: usize,
    published: usize,
    backpressured: bool,
}

struct SerialCountersAtomic {
    irq_total: AtomicUsize,
    irq_spurious: AtomicUsize,
    service_budget_exhausted: AtomicUsize,
    rx_bytes: AtomicUsize,
    rx_fifo_overruns: AtomicUsize,
    rx_queue_dropped: AtomicUsize,
    rx_breaks: AtomicUsize,
    rx_parity_errors: AtomicUsize,
    rx_framing_errors: AtomicUsize,
    tx_bytes: AtomicUsize,
}

impl SerialCountersAtomic {
    fn new() -> Self {
        Self {
            irq_total: AtomicUsize::new(0),
            irq_spurious: AtomicUsize::new(0),
            service_budget_exhausted: AtomicUsize::new(0),
            rx_bytes: AtomicUsize::new(0),
            rx_fifo_overruns: AtomicUsize::new(0),
            rx_queue_dropped: AtomicUsize::new(0),
            rx_breaks: AtomicUsize::new(0),
            rx_parity_errors: AtomicUsize::new(0),
            rx_framing_errors: AtomicUsize::new(0),
            tx_bytes: AtomicUsize::new(0),
        }
    }

    fn snapshot(&self) -> SerialCounters {
        SerialCounters {
            irq_total: self.irq_total.load(Ordering::Relaxed) as u64,
            irq_spurious: self.irq_spurious.load(Ordering::Relaxed) as u64,
            service_budget_exhausted: self.service_budget_exhausted.load(Ordering::Relaxed) as u64,
            rx_bytes: self.rx_bytes.load(Ordering::Relaxed) as u64,
            rx_fifo_overruns: self.rx_fifo_overruns.load(Ordering::Relaxed) as u64,
            rx_queue_dropped: self.rx_queue_dropped.load(Ordering::Relaxed) as u64,
            rx_breaks: self.rx_breaks.load(Ordering::Relaxed) as u64,
            rx_parity_errors: self.rx_parity_errors.load(Ordering::Relaxed) as u64,
            rx_framing_errors: self.rx_framing_errors.load(Ordering::Relaxed) as u64,
            tx_bytes: self.tx_bytes.load(Ordering::Relaxed) as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::VecDeque, sync::Arc, vec::Vec};
    use core::{
        num::NonZeroU32,
        sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    };

    use super::*;
    use crate::{DataBits, IrqSnapshot, Parity, RxSample, StopBits};

    struct MockProbe {
        enabled: AtomicBool,
        rx_reads_enabled: AtomicBool,
        irq_mask: AtomicU32,
        shutdowns: AtomicUsize,
        tx_writes: AtomicUsize,
        last_tx_byte: AtomicU32,
        tx_idle_polls: AtomicUsize,
    }

    impl MockProbe {
        fn new() -> Self {
            Self {
                enabled: AtomicBool::new(true),
                rx_reads_enabled: AtomicBool::new(true),
                irq_mask: AtomicU32::new(0),
                shutdowns: AtomicUsize::new(0),
                tx_writes: AtomicUsize::new(0),
                last_tx_byte: AtomicU32::new(0),
                tx_idle_polls: AtomicUsize::new(0),
            }
        }
    }

    struct MockUart {
        irq: VecDeque<IrqSnapshot>,
        rx: VecDeque<RxSample>,
        tx_ready_budget: usize,
        tx_load_size: usize,
        tx_written: Vec<u8>,
        tx_idle_after: usize,
        mask: InterruptMask,
        probe: Arc<MockProbe>,
    }

    impl MockUart {
        fn new() -> Self {
            Self {
                irq: VecDeque::new(),
                rx: VecDeque::new(),
                tx_ready_budget: 0,
                tx_load_size: 16,
                tx_written: Vec::new(),
                tx_idle_after: 0,
                mask: InterruptMask::empty(),
                probe: Arc::new(MockProbe::new()),
            }
        }

        fn with_probe() -> (Self, Arc<MockProbe>) {
            let uart = Self::new();
            let probe = Arc::clone(&uart.probe);
            (uart, probe)
        }

        fn irq(mut self, sources: IrqSource) -> Self {
            self.irq.push_back(IrqSnapshot {
                claimed: true,
                sources,
            });
            self
        }

        fn rx_byte(mut self, byte: u8) -> Self {
            self.rx.push_back(RxSample {
                byte: Some(byte),
                flag: RxFlag::Normal,
                overrun: false,
            });
            self
        }
    }

    impl RawUart for MockUart {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn base_addr(&self) -> usize {
            0x1000
        }

        fn clock_freq(&self) -> Option<NonZeroU32> {
            NonZeroU32::new(1)
        }

        fn startup(&mut self, _config: &Config) -> Result<(), ConfigError> {
            Ok(())
        }

        fn shutdown(&mut self) {
            self.probe.enabled.store(false, Ordering::Release);
            self.probe.shutdowns.fetch_add(1, Ordering::Relaxed);
        }

        fn set_config(&mut self, _config: &Config) -> Result<(), ConfigError> {
            Ok(())
        }

        fn baudrate(&self) -> u32 {
            115_200
        }

        fn data_bits(&self) -> DataBits {
            DataBits::Eight
        }

        fn stop_bits(&self) -> StopBits {
            StopBits::One
        }

        fn parity(&self) -> Parity {
            Parity::None
        }

        fn enable_loopback(&mut self) {}
        fn disable_loopback(&mut self) {}

        fn is_loopback_enabled(&self) -> bool {
            false
        }

        fn set_irq_mask(&mut self, mask: InterruptMask) {
            self.mask = mask;
            self.probe.irq_mask.store(mask.bits(), Ordering::Release);
        }

        fn take_irq_snapshot(&mut self) -> IrqSnapshot {
            self.irq.pop_front().unwrap_or_default()
        }

        fn read_rx(&mut self) -> Option<RxSample> {
            self.probe
                .rx_reads_enabled
                .load(Ordering::Acquire)
                .then(|| self.rx.pop_front())
                .flatten()
        }

        fn tx_ready(&mut self) -> bool {
            self.tx_ready_budget > 0
        }

        fn write_tx(&mut self, byte: u8) {
            assert!(self.tx_ready_budget > 0);
            self.tx_ready_budget -= 1;
            self.tx_written.push(byte);
            self.probe.tx_writes.fetch_add(1, Ordering::Relaxed);
            self.probe
                .last_tx_byte
                .store(u32::from(byte), Ordering::Relaxed);
        }

        fn tx_load_size(&self) -> usize {
            self.tx_load_size
        }

        fn tx_idle(&mut self) -> bool {
            let poll = self.probe.tx_idle_polls.fetch_add(1, Ordering::Relaxed);
            poll >= self.tx_idle_after
        }

        fn poll_status(&mut self) -> crate::SerialEvent {
            crate::SerialEvent::empty()
        }
    }

    fn start<const TX: usize, const RX: usize>(core: &mut SerialCore<TX, RX>) {
        core.startup(&Config::new()).unwrap();
        core.activate_interrupts().unwrap();
    }

    fn masked_capture<const TX: usize, const RX: usize>(
        core: &mut SerialCore<TX, RX>,
    ) -> (SerialIrqEvent, MaskedSource) {
        match core.capture_irq() {
            SerialIrqCapture::Captured {
                event,
                masked: Some(source),
            } => (event, source),
            other => panic!("expected masked IRQ capture, got {other:?}"),
        }
    }

    #[test]
    fn tx_queue_only_submits_software_work() {
        let parts = SerialCore::<8, 8>::split(MockUart::new());
        let mut tx = parts.tx;

        let submit = tx.submit(b"abc");

        assert_eq!(submit.accepted, 3);
        assert!(submit.needs_kick);
        assert_eq!(tx.chars_in_buffer(), 3);
    }

    #[test]
    fn emergency_write_is_bounded_and_bypasses_the_software_queue() {
        let (mut uart, probe) = MockUart::with_probe();
        uart.tx_ready_budget = EMERGENCY_TX_BUDGET + 8;
        let mut parts = SerialCore::<128, 8>::split(uart);
        start(&mut parts.core);
        let bytes = [b'x'; EMERGENCY_TX_BUDGET + 8];

        let result = parts.core.try_write_emergency(&bytes);

        assert_eq!(
            result,
            EmergencyWriteResult::Written {
                count: EMERGENCY_TX_BUDGET,
            }
        );
        assert_eq!(probe.tx_writes.load(Ordering::Relaxed), EMERGENCY_TX_BUDGET);
        assert_eq!(probe.last_tx_byte.load(Ordering::Relaxed), u32::from(b'x'));
        assert_eq!(parts.tx.chars_in_buffer(), 0);
    }

    #[test]
    fn emergency_write_rejects_a_non_running_port() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 1;
        let mut parts = SerialCore::<8, 8>::split(uart);

        assert_eq!(
            parts.core.try_write_emergency(b"x"),
            EmergencyWriteResult::Fault
        );
    }

    #[test]
    fn emergency_flush_holds_one_owner_until_the_transmitter_is_idle() {
        let (mut uart, probe) = MockUart::with_probe();
        uart.tx_idle_after = 3;
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        assert_eq!(
            parts.core.try_flush_emergency(),
            EmergencyFlushResult::Flushed
        );
        assert_eq!(probe.tx_idle_polls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn emergency_flush_stops_at_the_fixed_poll_budget() {
        let (mut uart, probe) = MockUart::with_probe();
        uart.tx_idle_after = usize::MAX;
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        assert_eq!(parts.core.try_flush_emergency(), EmergencyFlushResult::Busy);
        assert_eq!(
            probe.tx_idle_polls.load(Ordering::Relaxed),
            EMERGENCY_FLUSH_POLL_BUDGET
        );
    }

    #[test]
    fn split_erases_raw_type_and_keeps_capacity_generics() {
        let parts: SerialParts<8, 8> = SerialCore::split(MockUart::new());

        assert_eq!(parts.tx.write_room(), 7);
        assert!(!parts.rx.rx_pending());
    }

    #[test]
    fn startup_keeps_device_sources_masked_until_the_os_action_is_enabled() {
        let (uart, probe) = MockUart::with_probe();
        let mut parts = SerialCore::<8, 8>::split(uart);

        assert_eq!(
            parts.core.activate_interrupts(),
            Err(SerialActivationError::NotPrepared)
        );
        parts.core.startup(&Config::new()).unwrap();
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);

        assert_eq!(parts.core.activate_interrupts(), Ok(()));
        assert_eq!(
            probe.irq_mask.load(Ordering::Acquire),
            InterruptMask::RX.bits()
        );
    }

    #[test]
    fn quiesce_restores_polling_without_shutting_down_uart() {
        let (uart, probe) = MockUart::with_probe();
        let uart = uart.irq(IrqSource::RX_DATA).rx_byte(b'x');
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);
        let mut tx = parts.tx;
        tx.submit(b"pending");
        let (_, source) = masked_capture(&mut parts.core);
        assert!(matches!(
            parts.core.service_masked(source),
            SerialMaskedService::Complete(SerialIrqEvents { rx_pushed: 1, .. })
        ));
        let rx = parts.rx;
        assert!(rx.rx_pending());
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);

        parts.core.quiesce_to_polling();

        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        assert!(probe.enabled.load(Ordering::Acquire));
        assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 0);
        assert_eq!(parts.core.capture_irq(), SerialIrqCapture::Unhandled);
        assert_eq!(tx.chars_in_buffer(), 0);
        assert!(!rx.rx_pending());

        parts.core.shutdown();
        assert!(!probe.enabled.load(Ordering::Acquire));
        assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn bounded_service_flushes_tx_queue() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 3;
        let mut parts = SerialCore::<8, 8>::split(uart);
        let mut tx = parts.tx;
        tx.submit(b"abc");
        start(&mut parts.core);

        let events = parts.core.service(SerialSoftWork::TX_KICK).unwrap();

        assert_eq!(events.tx_sent, 3);
        assert!(events.tx_wakeup);
        assert_eq!(tx.chars_in_buffer(), 0);
    }

    #[test]
    fn tx_kick_wakes_again_when_queue_still_has_data() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 1;
        let mut parts = SerialCore::<8, 8>::split(uart);
        let mut tx = parts.tx;
        tx.submit(b"abc");
        start(&mut parts.core);

        let events = parts.core.service(SerialSoftWork::TX_KICK).unwrap();

        assert_eq!(events.tx_sent, 1);
        assert!(events.tx_wakeup);
        assert_eq!(tx.chars_in_buffer(), 2);
    }

    #[test]
    fn soft_tx_kick_never_exceeds_one_hardware_fifo_load() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = TX_KICK_BUDGET + 8;
        uart.tx_load_size = 1;
        let mut parts = SerialCore::<64, 8>::split(uart);
        let mut tx = parts.tx;
        let data = [b'x'; TX_KICK_BUDGET + 8];
        tx.submit(&data);
        start(&mut parts.core);

        let events = parts.core.service(SerialSoftWork::TX_KICK).unwrap();

        assert_eq!(events.tx_sent, 1);
        assert!(events.tx_wakeup);
        assert_eq!(tx.chars_in_buffer(), TX_KICK_BUDGET + 7);
    }

    #[test]
    fn zero_transmit_load_faults_and_masks_the_device() {
        let (mut uart, probe) = MockUart::with_probe();
        uart.tx_ready_budget = 1;
        uart.tx_load_size = 0;
        let mut parts = SerialCore::<8, 8>::split(uart);
        parts.tx.submit(b"x");
        start(&mut parts.core);

        let result = parts.core.service(SerialSoftWork::TX_KICK);

        assert_eq!(result, Err(SerialIrqFault::InvalidTransmitLoad));
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        assert_eq!(parts.core.capture_irq(), SerialIrqCapture::Unhandled);
    }

    #[test]
    fn irq_capture_masks_rx_without_servicing_the_fifo() {
        let uart = MockUart::new()
            .irq(IrqSource::RX_DATA)
            .rx_byte(b'A')
            .rx_byte(b'B');
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let (event, source) = masked_capture(&mut parts.core);

        assert_eq!(event.sources(), IrqSource::RX_DATA);
        assert!(!parts.rx.rx_pending());

        let service = parts.core.service_masked(source);
        assert_eq!(
            service,
            SerialMaskedService::Complete(SerialIrqEvents {
                rx_pushed: 2,
                ..SerialIrqEvents::default()
            })
        );
        let mut rx = parts.rx;
        let mut buf = [RxItem::default(); 2];
        assert_eq!(rx.drain(&mut buf).count, 2);
        assert_eq!(
            buf,
            [
                RxItem::Byte {
                    byte: b'A',
                    flag: RxFlag::Normal,
                },
                RxItem::Byte {
                    byte: b'B',
                    flag: RxFlag::Normal,
                },
            ]
        );
    }

    #[test]
    fn single_rx_irq_preserves_burst_until_owner_budget() {
        let mut uart = MockUart::new().irq(IrqSource::RX_DATA);
        let burst = b"echo 0123456789abcdefghijklmnopqrstuvwxyz\n";
        for &byte in burst {
            uart = uart.rx_byte(byte);
        }
        let mut parts = SerialCore::<8, 128>::split(uart);
        start(&mut parts.core);

        let (_, source) = masked_capture(&mut parts.core);
        let events = match parts.core.service_masked(source) {
            SerialMaskedService::Complete(events) => events,
            other => panic!("expected completed owner service, got {other:?}"),
        };

        assert_eq!(events.rx_pushed, burst.len());
        let mut rx = parts.rx;
        let mut items = [RxItem::default(); 64];
        assert_eq!(rx.drain(&mut items[..burst.len()]).count, burst.len());
        for (item, byte) in items.iter().zip(burst.iter()).take(burst.len()) {
            assert_eq!(
                *item,
                RxItem::Byte {
                    byte: *byte,
                    flag: RxFlag::Normal,
                }
            );
        }
    }

    #[test]
    fn rx_budget_counts_consumed_samples_not_published_items() {
        let mut uart = MockUart::new().irq(IrqSource::RX_STATUS);
        uart.rx.push_back(RxSample {
            byte: Some(b'x'),
            flag: RxFlag::Parity,
            overrun: true,
        });
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let (_, source) = masked_capture(&mut parts.core);
        let events = match parts.core.service_masked(source) {
            SerialMaskedService::Complete(events) => events,
            other => panic!("expected completed owner service, got {other:?}"),
        };

        assert_eq!(events.rx_pushed, 2);
    }

    #[test]
    fn capture_masks_exact_source_and_returns_stable_event() {
        let uart = MockUart::new().irq(IrqSource::RX_DATA);
        let probe = Arc::clone(&uart.probe);
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let (event, source) = masked_capture(&mut parts.core);

        assert_eq!(event.sources(), IrqSource::RX_DATA);
        assert_eq!(source.bitmap().get(), u64::from(InterruptMask::RX.bits()));
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
    }

    #[test]
    fn owner_service_requires_explicit_source_rearm() {
        let uart = MockUart::new().irq(IrqSource::RX_DATA);
        let probe = Arc::clone(&uart.probe);
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let (_event, source) = masked_capture(&mut parts.core);
        let progress = parts.core.service_masked(source);

        assert_eq!(
            progress,
            SerialMaskedService::Complete(SerialIrqEvents::default())
        );
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        assert_eq!(parts.core.rearm_masked(source), Ok(InterruptMask::RX));
        assert_eq!(
            probe.irq_mask.load(Ordering::Acquire),
            InterruptMask::RX.bits()
        );
    }

    #[test]
    fn publication_containment_returns_one_generation_checked_rearm_token() {
        let (uart, probe) = MockUart::with_probe();
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let source = IrqEndpoint::contain(&mut parts.core, ContainmentCause::PublicationFull)
            .expect("the active RX source can be device-masked");

        assert_eq!(source.bitmap().get(), u64::from(InterruptMask::RX.bits()));
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        assert_eq!(IrqSourceControl::rearm(&mut parts.core, source), Ok(()));
        assert_eq!(
            IrqSourceControl::rearm(&mut parts.core, source),
            Err(SerialRearmError::Stale)
        );
        assert_eq!(
            probe.irq_mask.load(Ordering::Acquire),
            InterruptMask::RX.bits()
        );
    }

    #[test]
    fn dropped_capture_leaves_source_safely_masked() {
        let uart = MockUart::new().irq(IrqSource::RX_DATA);
        let probe = Arc::clone(&uart.probe);
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let capture = masked_capture(&mut parts.core);
        let _ = capture;

        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        assert!(parts.core.service(SerialSoftWork::empty()).is_ok());
    }

    #[test]
    fn pending_owner_pass_keeps_source_masked() {
        let mut uart = MockUart::new().irq(IrqSource::RX_DATA);
        for _ in 0..(RX_IRQ_BUDGET * 3) {
            uart = uart.rx_byte(b'x');
        }
        let probe = Arc::clone(&uart.probe);
        let mut parts = SerialCore::<8, 1024>::split(uart);
        start(&mut parts.core);
        let (_event, source) = masked_capture(&mut parts.core);

        let progress = parts.core.service_masked(source);

        assert!(matches!(progress, SerialMaskedService::Pending(_)));
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
    }

    #[test]
    fn stale_capture_cannot_rearm_a_restarted_port() {
        let uart = MockUart::new().irq(IrqSource::RX_DATA);
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);
        let (_event, source) = masked_capture(&mut parts.core);

        parts.core.shutdown();

        assert_eq!(
            parts.core.rearm_masked(source),
            Err(SerialRearmError::Stale)
        );
    }

    #[test]
    fn rx_backpressure_stays_masked_until_the_consumer_rearms_it() {
        let uart = MockUart::new()
            .irq(IrqSource::RX_DATA)
            .rx_byte(b'a')
            .rx_byte(b'b');
        let probe = Arc::clone(&uart.probe);
        let mut parts = SerialCore::<8, 3>::split(uart);
        start(&mut parts.core);

        let (_, source) = masked_capture(&mut parts.core);
        let service = parts.core.service_masked(source);

        assert_eq!(
            service,
            SerialMaskedService::Backpressured(SerialIrqEvents {
                rx_pushed: 1,
                ..SerialIrqEvents::default()
            })
        );
        assert_eq!(source.bitmap().get(), u64::from(InterruptMask::RX.bits()));
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        let mut items = [RxItem::default(); 2];
        let drain = parts.rx.drain(&mut items);
        assert_eq!(drain.count, 1);
        let rearm = drain.rearm.unwrap();
        assert_eq!(rearm, source);
        assert_eq!(IrqSourceControl::rearm(&mut parts.core, rearm), Ok(()));
        assert_eq!(
            probe.irq_mask.load(Ordering::Acquire),
            InterruptMask::RX.bits()
        );
        assert!(parts.rx.drain(&mut items).rearm.is_none());
    }

    #[test]
    fn combined_masked_sources_progress_and_rearm_independently() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = TX_IRQ_BUDGET + 8;
        uart.tx_load_size = 1;
        uart.probe.rx_reads_enabled.store(false, Ordering::Release);
        uart = uart.irq(IrqSource::RX_DATA | IrqSource::TX_SPACE);
        uart = uart.rx_byte(b'a').rx_byte(b'b');
        let probe = Arc::clone(&uart.probe);
        let mut parts = SerialCore::<128, 3>::split(uart);
        let mut tx = parts.tx;
        tx.submit(&[b'x'; TX_IRQ_BUDGET + 4]);
        start(&mut parts.core);
        let _ = parts.core.service(SerialSoftWork::TX_KICK).unwrap();
        let (_event, source) = masked_capture(&mut parts.core);
        assert_eq!(
            source.bitmap().get(),
            u64::from((InterruptMask::RX | InterruptMask::TX_SPACE).bits())
        );
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        probe.rx_reads_enabled.store(true, Ordering::Release);

        let generation = usize::try_from(source.generation().get()).unwrap();
        let rx_source = masked_source(generation, InterruptMask::RX);
        let tx_source = masked_source(generation, InterruptMask::TX_SPACE);
        let rx_progress = parts.core.service_masked(rx_source);
        let tx_progress = parts.core.service_masked(tx_source);
        let tx_rearmed = parts.core.rearm_masked(tx_source);

        assert!(matches!(rx_progress, SerialMaskedService::Backpressured(_)));
        assert!(matches!(tx_progress, SerialMaskedService::Complete(_)));
        assert_eq!(tx_rearmed, Ok(InterruptMask::TX_SPACE));
        assert_eq!(
            probe.irq_mask.load(Ordering::Acquire),
            InterruptMask::TX_SPACE.bits()
        );
        assert_eq!(tx.chars_in_buffer(), TX_IRQ_BUDGET + 2);
    }

    #[test]
    fn busy_detect_irq_faults_instead_of_claiming_unmaskable_progress() {
        let (uart, probe) = MockUart::with_probe();
        let uart = uart.irq(IrqSource::BUSY_DETECT);
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let reason = match parts.core.capture_irq() {
            SerialIrqCapture::Fault {
                reason,
                containment: FaultContainment::Uncontained,
            } => reason,
            other => panic!("expected BUSY fail-closed service, got {other:?}"),
        };

        assert_eq!(reason, SerialIrqFault::UnmaskableSource);
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
    }

    #[test]
    fn unknown_irq_source_is_masked_instead_of_claimed_without_progress() {
        let (uart, probe) = MockUart::with_probe();
        let uart = uart.irq(IrqSource::OTHER_ACK);
        let mut parts = SerialCore::<8, 8>::split(uart);
        start(&mut parts.core);

        let mut irq = parts.core;
        let reason = match irq.capture_irq() {
            SerialIrqCapture::Fault {
                reason,
                containment: FaultContainment::Uncontained,
            } => reason,
            other => panic!("expected fail-closed IRQ service, got {other:?}"),
        };

        assert_eq!(reason, SerialIrqFault::UnknownSource);
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
    }
}
