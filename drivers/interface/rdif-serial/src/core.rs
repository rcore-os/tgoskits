use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::Cell,
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::{
    Config, ConfigError, InterruptMask, IrqSource, RawUart, RxFlag, RxItem, SerialCounters,
    SerialIrqFault, SerialIrqOutcome, SpscRing,
};

pub const DEFAULT_TX_CAP: usize = 4097;
pub const DEFAULT_RX_CAP: usize = 4097;

pub const RX_IRQ_BUDGET: usize = 256;
pub const TX_IRQ_BUDGET: usize = 64;
pub const IRQ_PASS_BUDGET: usize = 32;
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
    pub fn drain(&mut self, out: &mut [RxItem]) -> usize {
        let mut count = 0;
        for slot in out {
            let Some(item) = self.state.ring.pop() else {
                break;
            };
            *slot = item;
            count += 1;
        }
        count
    }

    /// Takes the generation whose RX source was masked for queue backpressure.
    pub fn take_rearm_request(&mut self) -> Option<RxRearmRequest> {
        let generation = self.state.rearm_generation.swap(0, Ordering::AcqRel);
        (generation != 0).then_some(RxRearmRequest { generation })
    }

    pub fn rx_pending(&self) -> bool {
        !self.state.ring.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PortState {
    Down,
    Polling,
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

/// Linear device-local continuation produced after exact UART source masking.
#[derive(Debug, Eq, PartialEq)]
#[must_use = "a serial continuation retains masked device interrupt sources"]
pub struct SerialContinuation {
    generation: usize,
    sources: InterruptMask,
}

/// Generation-bearing request to rearm RX after the consumer released space.
#[derive(Debug, Eq, PartialEq)]
#[must_use]
pub struct RxRearmRequest {
    generation: usize,
}

/// Result of one bounded device-local continuation pass.
#[derive(Debug, Eq, PartialEq)]
pub enum SerialContinuationProgress {
    Drained(SerialIrqOutcome),
    Pending {
        continuation: SerialContinuation,
        outcome: SerialIrqOutcome,
    },
    Backpressured(SerialIrqOutcome),
    Stale,
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

    /// Attempts a bounded emergency write while OS glue holds the port lock.
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

    /// Makes one bounded transmitter-drain attempt while holding the port lock.
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

    pub fn startup(&mut self, config: &Config) -> Result<SerialIrqOutcome, ConfigError> {
        if self.inner.state == PortState::Running {
            return Ok(SerialIrqOutcome::default());
        }

        self.bump_generation();
        if self.inner.state == PortState::Faulted {
            self.inner.raw.shutdown();
            self.inner.state = PortState::Down;
        }

        self.inner.raw.startup(config)?;
        self.inner.irq_mask = InterruptMask::RX;
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        self.inner.tx_irq_enabled = false;
        self.inner.rx_backpressured = false;
        self.rx.rearm_generation.store(0, Ordering::Release);
        self.inner.state = PortState::Running;
        Ok(SerialIrqOutcome::default())
    }

    pub fn shutdown(&mut self) {
        self.bump_generation();
        if self.inner.state == PortState::Down {
            return;
        }

        self.inner.raw.set_irq_mask(InterruptMask::empty());
        self.inner.raw.shutdown();
        self.inner.irq_mask = InterruptMask::empty();
        self.inner.tx_irq_enabled = false;
        self.inner.rx_backpressured = false;
        self.inner.state = PortState::Down;
        self.rx.rearm_generation.store(0, Ordering::Release);
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

    /// Captures and services a bounded hard-IRQ batch.
    pub fn handle_irq(&mut self) -> SerialIrqOutcome {
        let mut out = SerialIrqOutcome::default();
        if self.inner.state != PortState::Running {
            return out;
        }

        let mut rx_budget = RX_IRQ_BUDGET;
        let mut tx_budget = TX_IRQ_BUDGET;
        let mut source_drained = false;
        let mut observed_sources = IrqSource::empty();
        for _ in 0..IRQ_PASS_BUDGET {
            let snapshot = self.inner.raw.take_irq_snapshot();
            if !snapshot.claimed {
                if !out.claimed {
                    self.counters.irq_spurious.fetch_add(1, Ordering::Relaxed);
                }
                source_drained = true;
                break;
            }
            if !out.claimed {
                self.counters.irq_total.fetch_add(1, Ordering::Relaxed);
            }
            out.claimed = true;
            observed_sources |= snapshot.sources;

            if snapshot.sources.is_empty() || snapshot.sources.contains(IrqSource::OTHER_ACK) {
                self.quarantine(&mut out, SerialIrqFault::UnknownSource);
                break;
            }

            if snapshot
                .sources
                .intersects(IrqSource::RX_DATA | IrqSource::RX_TIMEOUT | IrqSource::RX_STATUS)
            {
                let service = self.service_rx(rx_budget);
                rx_budget = rx_budget.saturating_sub(service.consumed);
                out.rx_pushed += service.published;
                if service.backpressured {
                    out.rx_backpressured = true;
                    break;
                }
            }

            if snapshot.sources.contains(IrqSource::TX_SPACE) {
                let sent = self.service_tx(tx_budget, &mut out);
                tx_budget = tx_budget.saturating_sub(sent);
            }

            if snapshot.sources.contains(IrqSource::MODEM_STATUS) {
                self.inner.raw.ack_modem_status();
            }

            if snapshot.sources.contains(IrqSource::BUSY_DETECT) {
                self.inner.raw.ack_busy_detect();
            }

            if rx_budget == 0 || tx_budget == 0 {
                break;
            }
        }
        if out.claimed
            && out.fault.is_none()
            && !out.rx_backpressured
            && !source_drained
        {
            out.budget_exhausted = true;
            self.counters
                .irq_budget_exhausted
                .fetch_add(1, Ordering::Relaxed);
            self.defer_sources(observed_sources, &mut out);
        }
        out
    }

    /// Continues one exact-source masked IRQ generation in worker context.
    pub fn continue_irq(
        &mut self,
        continuation: SerialContinuation,
    ) -> SerialContinuationProgress {
        if continuation.generation != self.inner.generation
            || self.inner.state != PortState::Running
        {
            return SerialContinuationProgress::Stale;
        }

        let mut outcome = SerialIrqOutcome {
            claimed: true,
            ..SerialIrqOutcome::default()
        };
        let mut pending = false;
        if continuation.sources.intersects(InterruptMask::RX) {
            let service = self.service_rx(RX_IRQ_BUDGET);
            outcome.rx_pushed = service.published;
            if service.backpressured {
                outcome.rx_backpressured = true;
                return SerialContinuationProgress::Backpressured(outcome);
            }
            pending |= service.consumed == RX_IRQ_BUDGET;
        }
        if continuation.sources.contains(InterruptMask::TX_SPACE) {
            self.service_tx(TX_IRQ_BUDGET, &mut outcome);
        }
        if pending {
            outcome.budget_exhausted = true;
            return SerialContinuationProgress::Pending {
                continuation,
                outcome,
            };
        }

        self.rearm_continuation_sources(&continuation);
        SerialContinuationProgress::Drained(outcome)
    }

    /// Rearms RX after the sole consumer released software-ring capacity.
    pub fn rearm_rx(&mut self, request: RxRearmRequest) -> bool {
        if request.generation != self.inner.generation
            || self.inner.state != PortState::Running
            || !self.inner.rx_backpressured
        {
            return false;
        }
        self.inner.rx_backpressured = false;
        self.inner.irq_mask.insert(InterruptMask::RX);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        true
    }

    /// Performs bounded software-originated work without probing IRQ status.
    pub fn service(&mut self, work: SerialSoftWork) -> SerialIrqOutcome {
        let mut out = SerialIrqOutcome::default();
        if self.inner.state == PortState::Running && work.contains(SerialSoftWork::TX_KICK) {
            self.service_tx(TX_KICK_BUDGET, &mut out);
        }
        out
    }

    fn bump_generation(&mut self) {
        self.inner.generation = self
            .inner
            .generation
            .checked_add(1)
            .expect("serial generation exhausted");
    }

    fn defer_sources(&mut self, sources: IrqSource, outcome: &mut SerialIrqOutcome) {
        let source_mask = interrupt_mask_for_sources(sources);
        if source_mask.is_empty() {
            self.quarantine(outcome, SerialIrqFault::UnmaskableSource);
            return;
        }
        self.inner.irq_mask.remove(source_mask);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        outcome.continuation = Some(SerialContinuation {
            generation: self.inner.generation,
            sources: source_mask,
        });
    }

    fn rearm_continuation_sources(&mut self, continuation: &SerialContinuation) {
        let mut sources = continuation.sources;
        if self.tx.ring.is_empty() {
            sources.remove(InterruptMask::TX_SPACE);
            self.inner.tx_irq_enabled = false;
        }
        self.inner.irq_mask.insert(sources);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
    }

    fn service_rx(&mut self, budget: usize) -> RxService {
        let mut result = RxService::default();
        for _ in 0..budget {
            if self.rx.ring.remaining_snapshot() < 2 {
                self.mask_rx_for_backpressure();
                result.backpressured = true;
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
            if self
                .rx
                .push_from_owner(RxItem::Byte { byte, flag: sample.flag })
            {
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

    fn mask_rx_for_backpressure(&mut self) {
        self.inner.irq_mask.remove(InterruptMask::RX);
        self.inner.raw.set_irq_mask(self.inner.irq_mask);
        self.inner.rx_backpressured = true;
        self.rx
            .rearm_generation
            .store(self.inner.generation, Ordering::Release);
    }

    fn service_tx(
        &mut self,
        budget: usize,
        out: &mut SerialIrqOutcome,
    ) -> usize {
        let load_size = self.inner.raw.tx_load_size().max(1);
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

        if self.tx.ring.is_empty() {
            if self.inner.tx_irq_enabled {
                self.inner.irq_mask.remove(InterruptMask::TX_SPACE);
                self.inner.raw.set_irq_mask(self.inner.irq_mask);
                self.inner.tx_irq_enabled = false;
            }
        } else if !self.inner.tx_irq_enabled {
            self.inner.irq_mask.insert(InterruptMask::TX_SPACE);
            self.inner.raw.set_irq_mask(self.inner.irq_mask);
            self.inner.tx_irq_enabled = true;
        }

        if sent > 0 {
            self.tx.blocked.store(false, Ordering::Release);
            out.tx_wakeup = true;
        }
        out.tx_sent += sent;
        sent
    }

    fn quarantine(&mut self, outcome: &mut SerialIrqOutcome, fault: SerialIrqFault) {
        self.inner.raw.set_irq_mask(InterruptMask::empty());
        self.inner.irq_mask = InterruptMask::empty();
        self.inner.tx_irq_enabled = false;
        self.inner.rx_backpressured = false;
        self.inner.state = PortState::Faulted;
        outcome.fault = Some(fault);
    }
}

fn interrupt_mask_for_sources(sources: IrqSource) -> InterruptMask {
    let mut mask = InterruptMask::empty();
    if sources.intersects(IrqSource::RX_DATA | IrqSource::RX_TIMEOUT | IrqSource::RX_STATUS) {
        mask |= InterruptMask::RX;
    }
    if sources.contains(IrqSource::TX_SPACE) {
        mask |= InterruptMask::TX_SPACE;
    }
    if sources.contains(IrqSource::MODEM_STATUS) {
        mask |= InterruptMask::MODEM_STATUS;
    }
    mask
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
    irq_budget_exhausted: AtomicUsize,
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
            irq_budget_exhausted: AtomicUsize::new(0),
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
            irq_budget_exhausted: self.irq_budget_exhausted.load(Ordering::Relaxed) as u64,
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
            self.rx.pop_front()
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

    #[test]
    fn tx_queue_only_submits_software_work() {
        let mut parts = SerialCore::<8, 8>::split(MockUart::new());
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
        parts.core.startup(&Config::new()).unwrap();
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
        parts.core.startup(&Config::new()).unwrap();

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
        parts.core.startup(&Config::new()).unwrap();

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
    fn quiesce_restores_polling_without_shutting_down_uart() {
        let (uart, probe) = MockUart::with_probe();
        let uart = uart.irq(IrqSource::RX_DATA).rx_byte(b'x');
        let mut parts = SerialCore::<8, 8>::split(uart);
        parts.core.startup(&Config::new()).unwrap();
        let mut tx = parts.tx;
        tx.submit(b"pending");
        let mut irq = parts.core;
        assert_eq!(irq.handle_irq().rx_pushed, 1);
        let rx = parts.rx;
        assert!(rx.rx_pending());
        assert_eq!(
            probe.irq_mask.load(Ordering::Acquire),
            InterruptMask::RX.bits()
        );

        parts.core.quiesce_to_polling();

        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        assert!(probe.enabled.load(Ordering::Acquire));
        assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 0);
        assert_eq!(irq.handle_irq(), SerialIrqOutcome::default());
        assert_eq!(tx.chars_in_buffer(), 0);
        assert!(!rx.rx_pending());

        parts.core.shutdown();
        assert!(!probe.enabled.load(Ordering::Acquire));
        assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn owner_service_flushes_tx_queue() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 3;
        let mut parts = SerialCore::<8, 8>::split(uart);
        let mut tx = parts.tx;
        tx.submit(b"abc");
        parts.core.startup(&Config::new()).unwrap();

        let outcome = parts.core.service(SerialSoftWork::TX_KICK);

        assert_eq!(outcome.tx_sent, 3);
        assert!(outcome.tx_wakeup);
        assert_eq!(tx.chars_in_buffer(), 0);
    }

    #[test]
    fn tx_kick_wakes_again_when_queue_still_has_data() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 1;
        let mut parts = SerialCore::<8, 8>::split(uart);
        let mut tx = parts.tx;
        tx.submit(b"abc");
        parts.core.startup(&Config::new()).unwrap();

        let outcome = parts.core.service(SerialSoftWork::TX_KICK);

        assert_eq!(outcome.tx_sent, 1);
        assert!(outcome.tx_wakeup);
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
        parts.core.startup(&Config::new()).unwrap();

        let outcome = parts.core.service(SerialSoftWork::TX_KICK);

        assert_eq!(outcome.tx_sent, 1);
        assert!(outcome.tx_wakeup);
        assert_eq!(tx.chars_in_buffer(), TX_KICK_BUDGET + 7);
    }

    #[test]
    fn irq_services_rx_and_preserves_items_for_rx_queue() {
        let uart = MockUart::new()
            .irq(IrqSource::RX_DATA)
            .rx_byte(b'A')
            .rx_byte(b'B');
        let mut parts = SerialCore::<8, 8>::split(uart);
        parts.core.startup(&Config::new()).unwrap();

        let mut irq = parts.core;
        let outcome = irq.handle_irq();

        assert!(outcome.claimed);
        assert_eq!(outcome.rx_pushed, 2);
        let mut rx = parts.rx;
        let mut buf = [RxItem::default(); 2];
        assert_eq!(rx.drain(&mut buf), 2);
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
        parts.core.startup(&Config::new()).unwrap();

        let mut irq = parts.core;
        let outcome = irq.handle_irq();

        assert!(outcome.claimed);
        assert_eq!(outcome.rx_pushed, burst.len());
        let mut rx = parts.rx;
        let mut items = [RxItem::default(); 64];
        assert_eq!(rx.drain(&mut items[..burst.len()]), burst.len());
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
        parts.core.startup(&Config::new()).unwrap();

        let mut irq = parts.core;
        let outcome = irq.handle_irq();

        assert!(outcome.claimed);
        assert_eq!(outcome.rx_pushed, 2);
        assert!(!outcome.budget_exhausted);
    }

    #[test]
    fn pass_budget_requires_a_deferred_continuation_even_with_small_snapshots() {
        let mut uart = MockUart::new();
        for _ in 0..IRQ_PASS_BUDGET {
            uart = uart.irq(IrqSource::MODEM_STATUS);
        }
        let mut parts = SerialCore::<8, 8>::split(uart);
        parts.core.startup(&Config::new()).unwrap();

        let mut irq = parts.core;
        let outcome = irq.handle_irq();

        assert!(outcome.claimed);
        assert!(outcome.budget_exhausted);
    }

    #[test]
    fn continuation_rechecks_the_source_until_no_snapshot_is_pending() {
        let mut uart = MockUart::new();
        for _ in 0..=IRQ_PASS_BUDGET {
            uart = uart.irq(IrqSource::MODEM_STATUS);
        }
        let mut parts = SerialCore::<8, 8>::split(uart);
        parts.core.startup(&Config::new()).unwrap();
        let mut irq = parts.core;

        let first = irq.handle_irq();
        let second = irq.handle_irq();

        assert!(first.budget_exhausted);
        assert!(second.claimed);
        assert!(!second.budget_exhausted);
    }

    #[test]
    fn unknown_irq_source_is_masked_instead_of_claimed_without_progress() {
        let (uart, probe) = MockUart::with_probe();
        let uart = uart.irq(IrqSource::OTHER_ACK);
        let mut parts = SerialCore::<8, 8>::split(uart);
        parts.core.startup(&Config::new()).unwrap();

        let mut irq = parts.core;
        let outcome = irq.handle_irq();

        assert!(outcome.claimed);
        assert_eq!(outcome.fault, Some(SerialIrqFault::UnknownSource));
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
    }
}
