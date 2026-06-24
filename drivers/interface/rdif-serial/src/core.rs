use alloc::sync::Arc;
use core::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use crate::{
    Config, ConfigError, InterruptMask, IrqSource, RawUart, RxFlag, RxItem, SerialCounters,
    SerialIrqOutcome, SpscRing,
};

pub const DEFAULT_TX_CAP: usize = 4097;
pub const DEFAULT_RX_CAP: usize = 4097;

pub const RX_IRQ_BUDGET: usize = 256;
pub const TX_IRQ_BUDGET: usize = 64;
pub const IRQ_PASS_BUDGET: usize = 32;
pub const TX_KICK_BUDGET: usize = 32;

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
        debug_assert!(
            !self.active.swap(true, Ordering::AcqRel),
            "serial owner cell re-entered"
        );
        OwnerAccess { cell: self }
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
    Running,
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
        const RESERVICE = 1 << 1;
    }
}

pub trait TSerialIrqHandler: Send + Sync + 'static {
    fn owner(&self) -> OwnerId;
    fn handle(&self, lease: OwnerLease<'_>) -> SerialIrqOutcome;
    fn service(&self, lease: OwnerLease<'_>, work: SerialSoftWork) -> SerialIrqOutcome;
}

pub struct SerialParts<
    T: RawUart,
    const TX: usize = DEFAULT_TX_CAP,
    const RX: usize = DEFAULT_RX_CAP,
> {
    pub tx: TxQueue<TX>,
    pub rx: RxQueue<RX>,
    pub irq: Arc<SerialIrqHandler<T, TX, RX>>,
}

pub struct SerialIrqHandler<
    T: RawUart,
    const TX: usize = DEFAULT_TX_CAP,
    const RX: usize = DEFAULT_RX_CAP,
> {
    owner: OwnerId,
    core: Arc<OwnerCell<CoreInner<T>>>,
    tx: Arc<TxState<TX>>,
    rx: Arc<RxState<RX>>,
    counters: Arc<SerialCountersAtomic>,
}

impl<T: RawUart, const TX: usize, const RX: usize> SerialIrqHandler<T, TX, RX> {
    pub fn split(raw: T, owner: OwnerId) -> SerialParts<T, TX, RX> {
        let core = Arc::new(OwnerCell::new(CoreInner::new(raw)));
        let tx = Arc::new(TxState::new());
        let rx = Arc::new(RxState::new());
        let counters = Arc::new(SerialCountersAtomic::new());
        let irq = Arc::new(Self {
            owner,
            core,
            tx: tx.clone(),
            rx: rx.clone(),
            counters,
        });
        SerialParts {
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

    fn handle_locked(&self, core: &mut CoreInner<T>) -> SerialIrqOutcome {
        let mut out = SerialIrqOutcome::default();
        if core.state != PortState::Running {
            return out;
        }

        let mut rx_budget = RX_IRQ_BUDGET;
        let mut tx_budget = TX_IRQ_BUDGET;
        for _ in 0..IRQ_PASS_BUDGET {
            let snapshot = core.raw.take_irq_snapshot();
            if !snapshot.claimed {
                if !out.claimed {
                    self.counters.irq_spurious.fetch_add(1, Ordering::Relaxed);
                }
                break;
            }
            if !out.claimed {
                self.counters.irq_total.fetch_add(1, Ordering::Relaxed);
            }
            out.claimed = true;

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
                out.budget_exhausted = true;
                self.counters
                    .irq_budget_exhausted
                    .fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
        out
    }

    fn service_soft_locked(
        &self,
        core: &mut CoreInner<T>,
        work: SerialSoftWork,
    ) -> SerialIrqOutcome {
        let mut out = SerialIrqOutcome::default();
        if core.state != PortState::Running {
            return out;
        }

        if work.contains(SerialSoftWork::TX_KICK) {
            self.service_tx(core, TX_KICK_BUDGET, &mut out);
        }
        if work.contains(SerialSoftWork::RESERVICE) {
            let rx = self.service_rx(core, RX_IRQ_BUDGET);
            out.rx_pushed += rx.published;
            self.service_tx(core, TX_IRQ_BUDGET, &mut out);
        }
        out
    }

    fn service_rx(&self, core: &mut CoreInner<T>, budget: usize) -> RxService {
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
        core: &mut CoreInner<T>,
        budget: usize,
        out: &mut SerialIrqOutcome,
    ) -> usize {
        let limit = budget.min(core.raw.tx_load_size().max(1));
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

        if sent > 0 && self.tx.blocked.swap(false, Ordering::AcqRel) {
            out.tx_wakeup = true;
        }
        out.tx_sent += sent;
        sent
    }
}

impl<T: RawUart, const TX: usize, const RX: usize> TSerialIrqHandler
    for SerialIrqHandler<T, TX, RX>
{
    fn owner(&self) -> OwnerId {
        self.owner
    }

    fn handle(&self, mut lease: OwnerLease<'_>) -> SerialIrqOutcome {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        self.handle_locked(&mut core)
    }

    fn service(&self, mut lease: OwnerLease<'_>, work: SerialSoftWork) -> SerialIrqOutcome {
        self.assert_owner(&lease);
        let mut core = unsafe { self.core.access(&mut lease) };
        self.service_soft_locked(&mut core, work)
    }
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
    use alloc::{collections::VecDeque, vec::Vec};
    use core::num::NonZeroU32;

    use super::*;
    use crate::{DataBits, IrqSnapshot, Parity, RxSample, StopBits};

    struct MockUart {
        irq: VecDeque<IrqSnapshot>,
        rx: VecDeque<RxSample>,
        tx_ready_budget: usize,
        tx_written: Vec<u8>,
        mask: InterruptMask,
    }

    impl MockUart {
        fn new() -> Self {
            Self {
                irq: VecDeque::new(),
                rx: VecDeque::new(),
                tx_ready_budget: 0,
                tx_written: Vec::new(),
                mask: InterruptMask::empty(),
            }
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

        fn shutdown(&mut self) {}

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
        }

        fn tx_load_size(&self) -> usize {
            16
        }

        fn tx_idle(&mut self) -> bool {
            self.tx_written.is_empty()
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
        let parts = SerialIrqHandler::<MockUart, 8, 8>::split(MockUart::new(), OwnerId(0));
        let mut tx = parts.tx;

        let submit = tx.submit(b"abc");

        assert_eq!(submit.accepted, 3);
        assert!(submit.needs_kick);
        assert_eq!(tx.chars_in_buffer(), 3);
    }

    #[test]
    fn owner_service_flushes_tx_queue() {
        let mut uart = MockUart::new();
        uart.tx_ready_budget = 3;
        let parts = SerialIrqHandler::<MockUart, 8, 8>::split(uart, OwnerId(0));
        let mut tx = parts.tx;
        tx.submit(b"abc");
        parts.irq.startup(lease(), &Config::new()).unwrap();

        let outcome = parts.irq.service(lease(), SerialSoftWork::TX_KICK);

        assert_eq!(outcome.tx_sent, 3);
        assert_eq!(tx.chars_in_buffer(), 0);
    }

    #[test]
    fn irq_services_rx_and_preserves_items_for_rx_queue() {
        let uart = MockUart::new()
            .irq(IrqSource::RX_DATA)
            .rx_byte(b'A')
            .rx_byte(b'B');
        let parts = SerialIrqHandler::<MockUart, 8, 8>::split(uart, OwnerId(0));
        parts.irq.startup(lease(), &Config::new()).unwrap();

        let outcome = parts.irq.handle(lease());

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
        let parts = SerialIrqHandler::<MockUart, 8, 128>::split(uart, OwnerId(0));
        parts.irq.startup(lease(), &Config::new()).unwrap();

        let outcome = parts.irq.handle(lease());

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
        let parts = SerialIrqHandler::<MockUart, 8, 8>::split(uart, OwnerId(0));
        parts.irq.startup(lease(), &Config::new()).unwrap();

        let outcome = parts.irq.handle(lease());

        assert!(outcome.claimed);
        assert_eq!(outcome.rx_pushed, 2);
        assert!(!outcome.budget_exhausted);
    }
}
