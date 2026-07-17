use alloc::{boxed::Box, sync::Arc};
use core::{
    cell::{Cell, UnsafeCell},
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerId(pub usize);

pub struct OwnerLease<'a> {
    owner: OwnerId,
    _exclusive: PhantomData<&'a mut ()>,
    _not_sync: PhantomData<Cell<()>>,
}

impl<'a> OwnerLease<'a> {
    /// Creates an owner lease.
    ///
    /// # Safety
    ///
    /// The caller must be executing on `owner`, local IRQ/preemption state must
    /// prevent another owner lease from being created concurrently for the same
    /// UART, and no higher-priority FIQ/NMI/debug path may access this UART.
    pub unsafe fn new_unchecked(owner: OwnerId) -> Self {
        Self {
            owner,
            _exclusive: PhantomData,
            _not_sync: PhantomData,
        }
    }

    pub fn owner(&self) -> OwnerId {
        self.owner
    }
}

pub struct OwnerCell<T> {
    inner: UnsafeCell<T>,
    active: AtomicBool,
}

unsafe impl<T: Send> Send for OwnerCell<T> {}
unsafe impl<T: Send> Sync for OwnerCell<T> {}

impl<T> OwnerCell<T> {
    fn new(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(value),
            active: AtomicBool::new(false),
        }
    }

    unsafe fn access<'a>(&'a self, _lease: &'a mut OwnerLease<'_>) -> OwnerAccess<'a, T> {
        self.try_access().expect("serial owner cell re-entered")
    }

    fn try_access(&self) -> Option<OwnerAccess<'_, T>> {
        self.active
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| OwnerAccess { cell: self })
    }
}

pub struct OwnerAccess<'a, T> {
    cell: &'a OwnerCell<T>,
}

impl<T> core::ops::Deref for OwnerAccess<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.cell.inner.get() }
    }
}

impl<T> core::ops::DerefMut for OwnerAccess<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.cell.inner.get() }
    }
}

impl<T> Drop for OwnerAccess<'_, T> {
    fn drop(&mut self) {
        self.cell.active.store(false, Ordering::Release);
    }
}

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
}

impl<const N: usize> RxState<N> {
    fn new() -> Self {
        Self {
            ring: SpscRing::new(),
            pushed: AtomicUsize::new(0),
            dropped: AtomicUsize::new(0),
            overrun: AtomicUsize::new(0),
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
}

impl<T: RawUart> CoreInner<T> {
    fn new(raw: T) -> Self {
        Self {
            raw,
            irq_mask: InterruptMask::empty(),
            state: PortState::Down,
            tx_irq_enabled: false,
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
    pub port: Arc<SerialPort<TX, RX>>,
    pub tx: TxQueue<TX>,
    pub rx: RxQueue<RX>,
    pub irq: SerialIrqHandler<TX, RX>,
}

pub struct SerialPort<const TX: usize = DEFAULT_TX_CAP, const RX: usize = DEFAULT_RX_CAP> {
    owner: OwnerId,
    core: Arc<OwnerCell<CoreInner<DynRawUart>>>,
    tx: Arc<TxState<TX>>,
    rx: Arc<RxState<RX>>,
    counters: Arc<SerialCountersAtomic>,
}

pub struct SerialIrqHandler<const TX: usize = DEFAULT_TX_CAP, const RX: usize = DEFAULT_RX_CAP> {
    owner: OwnerId,
    core: Arc<OwnerCell<CoreInner<DynRawUart>>>,
    tx: Arc<TxState<TX>>,
    rx: Arc<RxState<RX>>,
    counters: Arc<SerialCountersAtomic>,
}

impl<const TX: usize, const RX: usize> SerialPort<TX, RX> {
    pub fn split(raw: impl RawUart, owner: OwnerId) -> SerialParts<TX, RX> {
        Self::split_boxed(Box::new(raw), owner)
    }

    pub fn split_boxed(raw: Box<dyn RawUart>, owner: OwnerId) -> SerialParts<TX, RX> {
        let core = Arc::new(OwnerCell::new(CoreInner::new(raw)));
        let tx = Arc::new(TxState::new());
        let rx = Arc::new(RxState::new());
        let counters = Arc::new(SerialCountersAtomic::new());
        let port = Arc::new(Self {
            owner,
            core: core.clone(),
            tx: tx.clone(),
            rx: rx.clone(),
            counters: counters.clone(),
        });
        let irq = SerialIrqHandler {
            owner,
            core: core.clone(),
            tx: tx.clone(),
            rx: rx.clone(),
            counters,
        };
        SerialParts {
            port,
            tx: TxQueue {
                state: tx,
                _single_producer: PhantomData,
            },
            rx: RxQueue {
                state: rx,
                _single_consumer: PhantomData,
            },
            irq,
        }
    }

    pub fn owner(&self) -> OwnerId {
        self.owner
    }

    /// Attempts a bounded emergency write directly to the runtime UART.
    ///
    /// This is the panic/diagnostic capability of the already-active runtime
    /// port. It performs one ownership CAS, does not allocate, spin, enqueue
    /// software work, acknowledge IRQ status, or invoke callbacks. A caller
    /// that can be interrupted by the normal UART owner should exclude that
    /// local interrupt while making the call. Concurrent and recursive owner
    /// attempts are rejected by the shared gate; an idle gate may be acquired
    /// from a remote CPU.
    pub fn try_write_emergency(&self, bytes: &[u8]) -> EmergencyWriteResult {
        if bytes.is_empty() {
            return EmergencyWriteResult::Written { count: 0 };
        }
        let Some(mut core) = self.core.try_access() else {
            return EmergencyWriteResult::Busy;
        };
        if core.state != PortState::Running {
            return EmergencyWriteResult::Fault;
        }

        let mut count = 0;
        while count < bytes.len().min(EMERGENCY_TX_BUDGET) && core.raw.tx_ready() {
            core.raw.write_tx(bytes[count]);
            count += 1;
        }
        if count == 0 {
            return EmergencyWriteResult::Busy;
        }
        self.counters.tx_bytes.fetch_add(count, Ordering::Relaxed);
        EmergencyWriteResult::Written { count }
    }

    /// Makes one bounded attempt to drain the runtime UART transmitter.
    ///
    /// The owner gate is acquired once and retained across at most
    /// [`EMERGENCY_FLUSH_POLL_BUDGET`] direct `tx_idle` status reads. This does
    /// not sleep, allocate, invoke callbacks, service IRQ state, or retry owner
    /// acquisition. `Busy` means either another owner is active or the
    /// transmitter did not become idle within the fixed budget. It is a
    /// shutdown/fatal-diagnostic drain, not a normal runtime polling path.
    pub fn try_flush_emergency(&self) -> EmergencyFlushResult {
        let Some(mut core) = self.core.try_access() else {
            return EmergencyFlushResult::Busy;
        };
        if core.state != PortState::Running {
            return EmergencyFlushResult::Fault;
        }
        for _ in 0..EMERGENCY_FLUSH_POLL_BUDGET {
            if core.raw.tx_idle() {
                return EmergencyFlushResult::Flushed;
            }
        }
        EmergencyFlushResult::Busy
    }

    pub fn startup(
        &self,
        mut lease: OwnerLease<'_>,
        config: &Config,
    ) -> Result<SerialIrqOutcome, ConfigError> {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        if core.state == PortState::Running {
            return Ok(SerialIrqOutcome::default());
        }

        if core.state == PortState::Faulted {
            core.raw.shutdown();
            core.state = PortState::Down;
        }

        core.raw.startup(config)?;
        core.irq_mask = InterruptMask::RX;
        let irq_mask = core.irq_mask;
        core.raw.set_irq_mask(irq_mask);
        core.tx_irq_enabled = false;
        core.state = PortState::Running;
        Ok(SerialIrqOutcome::default())
    }

    pub fn shutdown(&self, mut lease: OwnerLease<'_>) {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        if core.state == PortState::Down {
            return;
        }

        core.raw.set_irq_mask(InterruptMask::empty());
        core.raw.shutdown();
        core.irq_mask = InterruptMask::empty();
        core.tx_irq_enabled = false;
        core.state = PortState::Down;
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
    pub fn quiesce_to_polling(&self, mut lease: OwnerLease<'_>) {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };

        core.raw.set_irq_mask(InterruptMask::empty());
        core.irq_mask = InterruptMask::empty();
        core.tx_irq_enabled = false;
        core.state = PortState::Polling;
        self.tx.clear_from_owner();
        self.rx.clear_from_owner();
    }

    pub fn set_config(
        &self,
        mut lease: OwnerLease<'_>,
        config: &Config,
    ) -> Result<(), ConfigError> {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        let saved_mask = core.irq_mask;
        core.raw.set_irq_mask(InterruptMask::empty());
        let result = core.raw.set_config(config);
        core.raw.set_irq_mask(saved_mask);
        result
    }

    pub fn baudrate(&self, mut lease: OwnerLease<'_>) -> u32 {
        self.assert_owner(&lease);
        let core = unsafe { self.core.access(&mut lease) };
        core.raw.baudrate()
    }

    pub fn tx_idle(&self, mut lease: OwnerLease<'_>) -> bool {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        self.tx.ring.is_empty() && core.raw.tx_idle()
    }

    pub fn counters(&self) -> SerialCounters {
        self.counters.snapshot()
    }

    fn assert_owner(&self, lease: &OwnerLease<'_>) {
        assert_eq!(lease.owner(), self.owner);
    }
}

impl<const TX: usize, const RX: usize> SerialIrqHandler<TX, RX> {
    pub fn owner(&self) -> OwnerId {
        self.owner
    }

    pub fn handle(&mut self, mut lease: OwnerLease<'_>) -> SerialIrqOutcome {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        self.handle_locked(&mut core)
    }

    fn assert_owner(&self, lease: &OwnerLease<'_>) {
        assert_eq!(lease.owner(), self.owner);
    }

    fn handle_locked(&self, core: &mut CoreInner<DynRawUart>) -> SerialIrqOutcome {
        let mut out = SerialIrqOutcome::default();
        if core.state != PortState::Running {
            return out;
        }

        let mut rx_budget = RX_IRQ_BUDGET;
        let mut tx_budget = TX_IRQ_BUDGET;
        let mut source_drained = false;
        for _ in 0..IRQ_PASS_BUDGET {
            let snapshot = core.raw.take_irq_snapshot();
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

            if snapshot.sources.is_empty() || snapshot.sources.contains(IrqSource::OTHER_ACK) {
                quarantine_irq_source(core, &mut out, SerialIrqFault::UnknownSource);
                break;
            }

            if snapshot
                .sources
                .intersects(IrqSource::RX_DATA | IrqSource::RX_TIMEOUT | IrqSource::RX_STATUS)
            {
                let service = self.service_rx(core, rx_budget);
                rx_budget = rx_budget.saturating_sub(service.consumed);
                out.rx_pushed += service.published;
            }

            if snapshot.sources.contains(IrqSource::TX_SPACE) {
                let sent = self.service_tx(core, tx_budget, &mut out);
                tx_budget = tx_budget.saturating_sub(sent);
            }

            if snapshot.sources.contains(IrqSource::MODEM_STATUS) {
                core.raw.ack_modem_status();
            }

            if snapshot.sources.contains(IrqSource::BUSY_DETECT) {
                core.raw.ack_busy_detect();
            }

            if rx_budget == 0 || tx_budget == 0 {
                break;
            }
        }
        if out.claimed && out.fault.is_none() && !source_drained {
            out.budget_exhausted = true;
            self.counters
                .irq_budget_exhausted
                .fetch_add(1, Ordering::Relaxed);
        }
        out
    }

    fn service_rx(&self, core: &mut CoreInner<DynRawUart>, budget: usize) -> RxService {
        let mut result = RxService::default();
        for _ in 0..budget {
            let Some(sample) = core.raw.read_rx() else {
                break;
            };
            result.consumed += 1;

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
            };

            if let Some(byte) = sample.byte {
                self.counters.rx_bytes.fetch_add(1, Ordering::Relaxed);
                if self.rx.push_from_owner(RxItem::Byte {
                    byte,
                    flag: sample.flag,
                }) {
                    result.published += 1;
                } else {
                    self.counters
                        .rx_queue_dropped
                        .fetch_add(1, Ordering::Relaxed);
                }
            }

            if sample.overrun {
                self.rx.overrun.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .rx_fifo_overruns
                    .fetch_add(1, Ordering::Relaxed);
                if self.rx.push_from_owner(RxItem::Overrun) {
                    result.published += 1;
                } else {
                    self.counters
                        .rx_queue_dropped
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        result
    }

    fn service_tx(
        &self,
        core: &mut CoreInner<DynRawUart>,
        budget: usize,
        out: &mut SerialIrqOutcome,
    ) -> usize {
        let load_size = core.raw.tx_load_size();
        if load_size == 0 && !self.tx.ring.is_empty() {
            quarantine_irq_source(core, out, SerialIrqFault::InvalidTransmitLoad);
            return 0;
        }
        let limit = budget.min(load_size);
        let mut sent = 0;
        while sent < limit && core.raw.tx_ready() {
            let Some(byte) = self.tx.ring.peek_copy() else {
                break;
            };
            core.raw.write_tx(byte);
            let committed = self.tx.ring.pop();
            debug_assert_eq!(committed, Some(byte));
            self.tx.sent.fetch_add(1, Ordering::Relaxed);
            self.counters.tx_bytes.fetch_add(1, Ordering::Relaxed);
            sent += 1;
        }

        if self.tx.ring.is_empty() {
            if core.tx_irq_enabled {
                core.irq_mask.remove(InterruptMask::TX_SPACE);
                core.raw.set_irq_mask(core.irq_mask);
                core.tx_irq_enabled = false;
            }
        } else if !core.tx_irq_enabled {
            core.irq_mask.insert(InterruptMask::TX_SPACE);
            core.raw.set_irq_mask(core.irq_mask);
            core.tx_irq_enabled = true;
        }

        if sent > 0 {
            self.tx.blocked.store(false, Ordering::Release);
            out.tx_wakeup = true;
        }
        out.tx_sent += sent;
        sent
    }
}

impl<const TX: usize, const RX: usize> SerialPort<TX, RX> {
    fn service_soft_locked(
        &self,
        core: &mut CoreInner<DynRawUart>,
        work: SerialSoftWork,
    ) -> SerialIrqOutcome {
        let mut out = SerialIrqOutcome::default();
        if core.state != PortState::Running {
            return out;
        }

        if work.contains(SerialSoftWork::TX_KICK) {
            self.service_tx(core, TX_KICK_BUDGET, &mut out);
        }
        out
    }

    fn service_rx(&self, core: &mut CoreInner<DynRawUart>, budget: usize) -> RxService {
        let mut result = RxService::default();
        for _ in 0..budget {
            let Some(sample) = core.raw.read_rx() else {
                break;
            };
            result.consumed += 1;

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
            };

            if let Some(byte) = sample.byte {
                self.counters.rx_bytes.fetch_add(1, Ordering::Relaxed);
                if self.rx.push_from_owner(RxItem::Byte {
                    byte,
                    flag: sample.flag,
                }) {
                    result.published += 1;
                } else {
                    self.counters
                        .rx_queue_dropped
                        .fetch_add(1, Ordering::Relaxed);
                }
            }

            if sample.overrun {
                self.rx.overrun.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .rx_fifo_overruns
                    .fetch_add(1, Ordering::Relaxed);
                if self.rx.push_from_owner(RxItem::Overrun) {
                    result.published += 1;
                } else {
                    self.counters
                        .rx_queue_dropped
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        result
    }

    fn service_tx(
        &self,
        core: &mut CoreInner<DynRawUart>,
        budget: usize,
        out: &mut SerialIrqOutcome,
    ) -> usize {
        let load_size = core.raw.tx_load_size();
        if load_size == 0 && !self.tx.ring.is_empty() {
            quarantine_irq_source(core, out, SerialIrqFault::InvalidTransmitLoad);
            return 0;
        }
        let limit = budget.min(load_size);
        let mut sent = 0;
        while sent < limit && core.raw.tx_ready() {
            let Some(byte) = self.tx.ring.peek_copy() else {
                break;
            };
            core.raw.write_tx(byte);
            let committed = self.tx.ring.pop();
            debug_assert_eq!(committed, Some(byte));
            self.tx.sent.fetch_add(1, Ordering::Relaxed);
            self.counters.tx_bytes.fetch_add(1, Ordering::Relaxed);
            sent += 1;
        }

        if self.tx.ring.is_empty() {
            if core.tx_irq_enabled {
                core.irq_mask.remove(InterruptMask::TX_SPACE);
                core.raw.set_irq_mask(core.irq_mask);
                core.tx_irq_enabled = false;
            }
        } else if !core.tx_irq_enabled {
            core.irq_mask.insert(InterruptMask::TX_SPACE);
            core.raw.set_irq_mask(core.irq_mask);
            core.tx_irq_enabled = true;
        }

        if sent > 0 {
            self.tx.blocked.store(false, Ordering::Release);
            out.tx_wakeup = true;
        }
        out.tx_sent += sent;
        sent
    }

    pub fn service(&self, mut lease: OwnerLease<'_>, work: SerialSoftWork) -> SerialIrqOutcome {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        self.service_soft_locked(&mut core, work)
    }
}

fn quarantine_irq_source(
    core: &mut CoreInner<DynRawUart>,
    outcome: &mut SerialIrqOutcome,
    fault: SerialIrqFault,
) {
    core.raw.set_irq_mask(InterruptMask::empty());
    core.irq_mask = InterruptMask::empty();
    core.tx_irq_enabled = false;
    core.state = PortState::Faulted;
    outcome.fault = Some(fault);
}

#[derive(Default)]
struct RxService {
    consumed: usize,
    published: usize,
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

    fn lease() -> OwnerLease<'static> {
        unsafe { OwnerLease::new_unchecked(OwnerId(0)) }
    }

    #[test]
    fn tx_queue_only_submits_software_work() {
        let parts = SerialPort::<8, 8>::split(MockUart::new(), OwnerId(0));
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
        let parts = SerialPort::<128, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();
        let bytes = [b'x'; EMERGENCY_TX_BUDGET + 8];

        let result = parts.port.try_write_emergency(&bytes);

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
    fn emergency_write_returns_busy_during_owner_reentry() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 1;
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();
        let mut owner_lease = lease();
        let _active = unsafe { parts.port.core.access(&mut owner_lease) };

        assert_eq!(
            parts.port.try_write_emergency(b"x"),
            EmergencyWriteResult::Busy
        );
    }

    #[test]
    fn emergency_write_rejects_a_non_running_port() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 1;
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));

        assert_eq!(
            parts.port.try_write_emergency(b"x"),
            EmergencyWriteResult::Fault
        );
    }

    #[test]
    fn emergency_flush_holds_one_owner_until_the_transmitter_is_idle() {
        let (mut uart, probe) = MockUart::with_probe();
        uart.tx_idle_after = 3;
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        assert_eq!(
            parts.port.try_flush_emergency(),
            EmergencyFlushResult::Flushed
        );
        assert_eq!(probe.tx_idle_polls.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn emergency_flush_stops_at_the_fixed_poll_budget() {
        let (mut uart, probe) = MockUart::with_probe();
        uart.tx_idle_after = usize::MAX;
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        assert_eq!(parts.port.try_flush_emergency(), EmergencyFlushResult::Busy);
        assert_eq!(
            probe.tx_idle_polls.load(Ordering::Relaxed),
            EMERGENCY_FLUSH_POLL_BUDGET
        );
    }

    #[test]
    fn split_erases_raw_type_and_keeps_capacity_generics() {
        let parts: SerialParts<8, 8> = SerialPort::split(MockUart::new(), OwnerId(0));

        assert_eq!(parts.port.owner(), OwnerId(0));
        assert_eq!(parts.irq.owner(), OwnerId(0));
        assert_eq!(parts.tx.write_room(), 7);
        assert!(!parts.rx.rx_pending());
    }

    #[test]
    fn quiesce_restores_polling_without_shutting_down_uart() {
        let (uart, probe) = MockUart::with_probe();
        let uart = uart.irq(IrqSource::RX_DATA).rx_byte(b'x');
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();
        let mut tx = parts.tx;
        tx.submit(b"pending");
        let mut irq = parts.irq;
        assert_eq!(irq.handle(lease()).rx_pushed, 1);
        let rx = parts.rx;
        assert!(rx.rx_pending());
        assert_eq!(
            probe.irq_mask.load(Ordering::Acquire),
            InterruptMask::RX.bits()
        );

        parts.port.quiesce_to_polling(lease());

        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
        assert!(probe.enabled.load(Ordering::Acquire));
        assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 0);
        assert_eq!(irq.handle(lease()), SerialIrqOutcome::default());
        assert_eq!(tx.chars_in_buffer(), 0);
        assert!(!rx.rx_pending());

        parts.port.shutdown(lease());
        assert!(!probe.enabled.load(Ordering::Acquire));
        assert_eq!(probe.shutdowns.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn owner_service_flushes_tx_queue() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 3;
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        let mut tx = parts.tx;
        tx.submit(b"abc");
        parts.port.startup(lease(), &Config::new()).unwrap();

        let outcome = parts.port.service(lease(), SerialSoftWork::TX_KICK);

        assert_eq!(outcome.tx_sent, 3);
        assert!(outcome.tx_wakeup);
        assert_eq!(tx.chars_in_buffer(), 0);
    }

    #[test]
    fn tx_kick_wakes_again_when_queue_still_has_data() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 1;
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        let mut tx = parts.tx;
        tx.submit(b"abc");
        parts.port.startup(lease(), &Config::new()).unwrap();

        let outcome = parts.port.service(lease(), SerialSoftWork::TX_KICK);

        assert_eq!(outcome.tx_sent, 1);
        assert!(outcome.tx_wakeup);
        assert_eq!(tx.chars_in_buffer(), 2);
    }

    #[test]
    fn soft_tx_kick_never_exceeds_one_hardware_fifo_load() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = TX_KICK_BUDGET + 8;
        uart.tx_load_size = 1;
        let parts = SerialPort::<64, 8>::split(uart, OwnerId(0));
        let mut tx = parts.tx;
        let data = [b'x'; TX_KICK_BUDGET + 8];
        tx.submit(&data);
        parts.port.startup(lease(), &Config::new()).unwrap();

        let outcome = parts.port.service(lease(), SerialSoftWork::TX_KICK);

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
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        let mut irq = parts.irq;
        let outcome = irq.handle(lease());

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
        let parts = SerialPort::<8, 128>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        let mut irq = parts.irq;
        let outcome = irq.handle(lease());

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
    fn soft_reservice_reports_work_remaining_after_its_budget() {
        let mut uart = MockUart::new();
        for _ in 0..=RX_IRQ_BUDGET {
            uart = uart.rx_byte(b'x');
        }
        let parts = SerialPort::<8, 512>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        let first = parts.port.service(lease(), SerialSoftWork::RESERVICE);
        let second = parts.port.service(lease(), SerialSoftWork::RESERVICE);

        assert_eq!(first.rx_pushed, RX_IRQ_BUDGET);
        assert!(first.budget_exhausted);
        assert_eq!(second.rx_pushed, 1);
        assert!(!second.budget_exhausted);
    }

    #[test]
    fn soft_reservice_reports_tx_work_remaining_after_its_budget() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = TX_IRQ_BUDGET + 1;
        let parts = SerialPort::<128, 8>::split(uart, OwnerId(0));
        let mut tx = parts.tx;
        let data = [b'x'; TX_IRQ_BUDGET + 1];
        assert_eq!(tx.submit(&data).accepted, data.len());
        parts.port.startup(lease(), &Config::new()).unwrap();

        let first = parts.port.service(lease(), SerialSoftWork::RESERVICE);
        assert_eq!(first.tx_sent, TX_IRQ_BUDGET);
        assert!(first.budget_exhausted);
        assert_eq!(tx.chars_in_buffer(), 1);

        let second = parts.port.service(lease(), SerialSoftWork::RESERVICE);
        assert_eq!(second.tx_sent, 1);
        assert!(!second.budget_exhausted);
        assert_eq!(tx.chars_in_buffer(), 0);
    }

    #[test]
    fn rx_budget_counts_consumed_samples_not_published_items() {
        let mut uart = MockUart::new().irq(IrqSource::RX_STATUS);
        uart.rx.push_back(RxSample {
            byte: Some(b'x'),
            flag: RxFlag::Parity,
            overrun: true,
        });
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        let mut irq = parts.irq;
        let outcome = irq.handle(lease());

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
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        let mut irq = parts.irq;
        let outcome = irq.handle(lease());

        assert!(outcome.claimed);
        assert!(outcome.budget_exhausted);
    }

    #[test]
    fn unknown_irq_source_is_masked_instead_of_claimed_without_progress() {
        let (uart, probe) = MockUart::with_probe();
        let uart = uart.irq(IrqSource::OTHER_ACK);
        let parts = SerialPort::<8, 8>::split(uart, OwnerId(0));
        parts.port.startup(lease(), &Config::new()).unwrap();

        let mut irq = parts.irq;
        let outcome = irq.handle(lease());

        assert!(outcome.claimed);
        assert_eq!(probe.irq_mask.load(Ordering::Acquire), 0);
    }
}
