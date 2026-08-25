//! Queue-level, fixed-CPU network poll runtime.
//!
//! Every physical IRQ source is assigned to an affinity domain.  A domain's
//! hard callbacks and queue processing run on one owner CPU; only move-only DMA
//! tokens cross the SPSC boundary to the single protocol executor.

use alloc::{boxed::Box, collections::VecDeque, format, string::String, sync::Arc, vec, vec::Vec};
use core::{
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
};

use ax_sync::SpinLock;
use ax_task::WaitQueue;
use irq_framework::IrqId;
use rd_net::{
    DmaBuffer, NetError, NetHardIrqEndpoint, NetHardIrqResult, NetIrqSourceId, NetRearmResult,
    PreparedNetDevice, PreparedNetPollGroup, RxCompletion, WifiLinkPolicy, WifiTransaction,
};

use crate::device::{
    EthernetFramePort, EthernetFramePortList, NetDeviceError, NetDeviceResult,
    ProtocolEthernetFrame,
};

const QUEUE_BUDGET: usize = 64;
const CPU_ROUND_BUDGET: usize = 256;
const WIFI_CONTROL_QUEUE_CAPACITY: usize = 8;

const STATE_IDLE: u8 = 0;
const STATE_SCHEDULED: u8 = 1;
const STATE_POLLING: u8 = 2;
const STATE_DISABLED: u8 = 3;
const STATE_MASK: u8 = 0x0f;
const STATE_MISSED: u8 = 0x80;

const COMMAND_WAIT: u8 = 0;
const COMMAND_START: u8 = 1;
const COMMAND_STOP: u8 = 2;

const STATUS_PENDING: u8 = 0;
const STATUS_READY: u8 = 1;
const STATUS_FAILED: u8 = 2;

struct WifiCommandCompletion {
    result: SpinLock<Option<Result<(), NetError>>>,
    wait: WaitQueue,
}

impl WifiCommandCompletion {
    fn new() -> Self {
        Self {
            result: SpinLock::new(None),
            wait: WaitQueue::new(),
        }
    }

    fn complete(&self, result: Result<(), NetError>) {
        *self.result.lock_irqsave() = Some(result);
        self.wait.notify_all(true);
    }

    fn wait(&self) -> Result<(), NetError> {
        self.wait
            .wait_until(|| self.result.lock_irqsave().is_some());
        self.result
            .lock_irqsave()
            .take()
            .expect("Wi-Fi completion was published without a result")
    }
}

struct WifiControlRequest {
    transaction: WifiTransaction,
    completion: Arc<WifiCommandCompletion>,
}

struct WifiControlQueue {
    requests: SpinLock<VecDeque<WifiControlRequest>>,
    stopped: AtomicBool,
}

impl WifiControlQueue {
    fn new() -> Self {
        Self {
            requests: SpinLock::new(VecDeque::with_capacity(WIFI_CONTROL_QUEUE_CAPACITY)),
            stopped: AtomicBool::new(false),
        }
    }

    fn submit(
        &self,
        transaction: WifiTransaction,
        notify: &ax_task::IrqNotify,
    ) -> Result<(), NetError> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(NetError::Stopped);
        }
        let completion = Arc::new(WifiCommandCompletion::new());
        {
            let mut requests = self.requests.lock_irqsave();
            if self.stopped.load(Ordering::Acquire) {
                return Err(NetError::Stopped);
            }
            if requests.len() == WIFI_CONTROL_QUEUE_CAPACITY {
                return Err(NetError::Retry);
            }
            requests.push_back(WifiControlRequest {
                transaction,
                completion: Arc::clone(&completion),
            });
        }
        notify.notify();
        completion.wait()
    }

    fn try_pop(&self) -> Option<WifiControlRequest> {
        self.requests.lock_irqsave().pop_front()
    }

    fn has_pending(&self) -> bool {
        !self.requests.lock_irqsave().is_empty()
    }

    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let pending = core::mem::take(&mut *self.requests.lock_irqsave());
        for request in pending {
            request.completion.complete(Err(NetError::Stopped));
        }
    }
}

#[derive(Clone)]
pub(crate) struct WifiRuntimeHandle {
    device_index: usize,
    owner_cpu: usize,
    queue: Arc<WifiControlQueue>,
    notify: Arc<ax_task::IrqNotify>,
}

impl WifiRuntimeHandle {
    pub(crate) const fn device_index(&self) -> usize {
        self.device_index
    }

    pub(crate) const fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }

    pub(crate) fn submit(&self, transaction: WifiTransaction) -> Result<(), NetError> {
        self.queue.submit(transaction, &self.notify)
    }
}

/// Runtime initialization or lifecycle error.
#[derive(Debug, thiserror::Error)]
pub enum NetworkRuntimeError {
    #[error("network device parts are inconsistent with their IRQ bindings")]
    InvalidTopology,
    #[error("network queue executor could not be pinned to CPU {0}")]
    WorkerAffinity(usize),
    #[error("network queue initialization failed")]
    QueueInit,
    #[error("network IRQ registration failed: {0}")]
    IrqRegistration(#[from] PinnedNetIrqError),
    #[error("network DMA setup failed: {0}")]
    Device(#[from] NetError),
}

/// Resolved driver source-id to physical IRQ mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedNetIrqSource {
    pub source_id: NetIrqSourceId,
    pub irq: IrqId,
}

/// One prepared device and its complete, resolved IRQ source map.
pub struct NetworkDeviceInput {
    pub name: String,
    pub device: PreparedNetDevice,
    pub irq_sources: Vec<ResolvedNetIrqSource>,
}

/// Result of a bounded hard-IRQ callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinnedNetIrqOutcome {
    Unhandled,
    Handled,
    Wake,
}

/// Move-only callback installed by the OS IRQ adapter.
pub struct PinnedNetIrqAction {
    handler: Box<dyn FnMut() -> PinnedNetIrqOutcome + Send>,
}

impl PinnedNetIrqAction {
    pub fn new(handler: impl FnMut() -> PinnedNetIrqOutcome + Send + 'static) -> Self {
        Self {
            handler: Box::new(handler),
        }
    }

    pub fn run(&mut self) -> PinnedNetIrqOutcome {
        (self.handler)()
    }
}

/// OS-specific fixed-affinity IRQ registration error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PinnedNetIrqError {
    #[error("invalid network IRQ or owner CPU")]
    Invalid,
    #[error("network IRQ affinity conflicts with an existing shared action")]
    AffinityConflict,
    #[error("fixed network IRQ routing is unsupported")]
    Unsupported,
    #[error("network IRQ operation failed")]
    Other,
}

/// Move-only registration lease.  It is created disabled.
pub trait PinnedNetIrqRegistration: Send + 'static {
    fn owner_cpu(&self) -> usize;
    fn enable(&self) -> Result<(), PinnedNetIrqError>;
    fn disable_and_synchronize(&self) -> Result<(), PinnedNetIrqError>;
}

/// OS adapter that accepts fixed affinity only.
pub trait PinnedNetIrqRegistrar: Sync {
    fn register(
        &self,
        name: String,
        irq: IrqId,
        owner_cpu: usize,
        action: PinnedNetIrqAction,
    ) -> Result<Box<dyn PinnedNetIrqRegistration>, PinnedNetIrqError>;
}

/// Observable queue statistics used by SMP contract tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NetQueueStats {
    pub irq: u64,
    pub schedule: u64,
    pub missed: u64,
    pub poll_batches: u64,
    pub budget_exhaustion: u64,
    pub spurious: u64,
    pub probe_deferred: u64,
    pub rearm_race: u64,
    pub owner_cpu: usize,
    pub last_irq_cpu: Option<usize>,
    pub last_poll_cpu: Option<usize>,
    pub irq_to_poll_remote_wake: u64,
}

struct QueueStatsAtomic {
    irq: AtomicU64,
    schedule: AtomicU64,
    missed: AtomicU64,
    poll_batches: AtomicU64,
    budget_exhaustion: AtomicU64,
    spurious: AtomicU64,
    probe_deferred: AtomicU64,
    rearm_race: AtomicU64,
    last_irq_cpu: AtomicUsize,
    last_poll_cpu: AtomicUsize,
    irq_to_poll_remote_wake: AtomicU64,
}

impl QueueStatsAtomic {
    const fn new() -> Self {
        Self {
            irq: AtomicU64::new(0),
            schedule: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            poll_batches: AtomicU64::new(0),
            budget_exhaustion: AtomicU64::new(0),
            spurious: AtomicU64::new(0),
            probe_deferred: AtomicU64::new(0),
            rearm_race: AtomicU64::new(0),
            last_irq_cpu: AtomicUsize::new(usize::MAX),
            last_poll_cpu: AtomicUsize::new(usize::MAX),
            irq_to_poll_remote_wake: AtomicU64::new(0),
        }
    }

    fn snapshot(&self, owner_cpu: usize) -> NetQueueStats {
        let optional_cpu = |cpu| (cpu != usize::MAX).then_some(cpu);
        NetQueueStats {
            irq: self.irq.load(Ordering::Relaxed),
            schedule: self.schedule.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            poll_batches: self.poll_batches.load(Ordering::Relaxed),
            budget_exhaustion: self.budget_exhaustion.load(Ordering::Relaxed),
            spurious: self.spurious.load(Ordering::Relaxed),
            probe_deferred: self.probe_deferred.load(Ordering::Relaxed),
            rearm_race: self.rearm_race.load(Ordering::Relaxed),
            owner_cpu,
            last_irq_cpu: optional_cpu(self.last_irq_cpu.load(Ordering::Acquire)),
            last_poll_cpu: optional_cpu(self.last_poll_cpu.load(Ordering::Acquire)),
            irq_to_poll_remote_wake: self.irq_to_poll_remote_wake.load(Ordering::Relaxed),
        }
    }
}

/// Shared atomic state for one poll group.
struct PollGroupState {
    state: AtomicU8,
    owner_cpu: usize,
    notify: Arc<ax_task::IrqNotify>,
    stats: QueueStatsAtomic,
}

impl PollGroupState {
    fn new(owner_cpu: usize, notify: Arc<ax_task::IrqNotify>) -> Self {
        Self {
            state: AtomicU8::new(STATE_DISABLED),
            owner_cpu,
            notify,
            stats: QueueStatsAtomic::new(),
        }
    }

    fn activate(&self, pending: bool) {
        self.state.store(STATE_IDLE, Ordering::Release);
        if pending {
            self.schedule_task();
        }
    }

    fn schedule_irq(&self) {
        let cpu = ax_hal::percpu::this_cpu_id();
        self.stats.irq.fetch_add(1, Ordering::Relaxed);
        self.stats.last_irq_cpu.store(cpu, Ordering::Release);
        if cpu != self.owner_cpu {
            self.stats
                .irq_to_poll_remote_wake
                .fetch_add(1, Ordering::Relaxed);
            self.disable();
            return;
        }
        if self.publish_schedule() {
            self.notify.notify_irq();
        }
    }

    fn schedule_task(&self) {
        self.publish_schedule();
        // A task-side publication can be what releases a queue executor that
        // stopped on RX/TX ring backpressure.  In that case the state is
        // POLLING|MISSED rather than a fresh IDLE->SCHEDULED transition, but
        // the sleeping owner still needs a precise wakeup.
        if !self.is_disabled() {
            self.notify.notify();
        }
    }

    fn publish_schedule(&self) -> bool {
        loop {
            let old = self.state.load(Ordering::Acquire);
            match old & STATE_MASK {
                STATE_DISABLED => return false,
                STATE_IDLE => {
                    if self
                        .state
                        .compare_exchange(old, STATE_SCHEDULED, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        self.stats.schedule.fetch_add(1, Ordering::Relaxed);
                        return true;
                    }
                }
                STATE_SCHEDULED | STATE_POLLING => {
                    if old & STATE_MISSED != 0 {
                        return false;
                    }
                    if self
                        .state
                        .compare_exchange(
                            old,
                            old | STATE_MISSED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        self.stats.missed.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }

    fn claim(&self) -> bool {
        let current_cpu = ax_hal::percpu::this_cpu_id();
        if current_cpu != self.owner_cpu {
            self.disable();
            return false;
        }
        loop {
            let old = self.state.load(Ordering::Acquire);
            let claimable = (old & STATE_MASK == STATE_SCHEDULED)
                || (old & STATE_MASK == STATE_POLLING && old & STATE_MISSED != 0);
            if !claimable {
                return false;
            }
            if self
                .state
                .compare_exchange(old, STATE_POLLING, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.stats
                    .last_poll_cpu
                    .store(current_cpu, Ordering::Release);
                self.stats.poll_batches.fetch_add(1, Ordering::Relaxed);
                return true;
            }
        }
    }

    fn finish_more(&self) {
        loop {
            let old = self.state.load(Ordering::Acquire);
            if old & STATE_MASK != STATE_POLLING {
                return;
            }
            if self
                .state
                .compare_exchange(old, STATE_SCHEDULED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    fn begin_rearm(&self) -> bool {
        loop {
            let old = self.state.load(Ordering::Acquire);
            if old & STATE_MASK != STATE_POLLING {
                return false;
            }
            if old & STATE_MISSED != 0 {
                if self
                    .state
                    .compare_exchange(old, STATE_SCHEDULED, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return false;
                }
                continue;
            }
            if self
                .state
                .compare_exchange(old, STATE_IDLE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn disable(&self) {
        self.state.store(STATE_DISABLED, Ordering::Release);
        self.notify.notify();
    }

    fn is_disabled(&self) -> bool {
        self.state.load(Ordering::Acquire) & STATE_MASK == STATE_DISABLED
    }
}

/// Heap-backed SPSC core.  One slot is reserved to distinguish full/empty.
struct SpscCore<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: only the unique producer writes at `tail`, only the unique consumer
// reads/drops at `head`, and Acquire/Release publication prevents aliasing a
// live slot. `T: Send` is the required cross-CPU ownership contract.
unsafe impl<T: Send> Sync for SpscCore<T> {}
unsafe impl<T: Send> Send for SpscCore<T> {}

impl<T> Drop for SpscCore<T> {
    fn drop(&mut self) {
        let mut head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        while head != tail {
            // SAFETY: after both endpoints are dropped no concurrent access
            // remains, and every index in [head, tail) contains one live item.
            unsafe { (*self.slots[head].get()).assume_init_drop() };
            head = (head + 1) % self.slots.len();
        }
    }
}

struct SpscProducer<T> {
    core: Arc<SpscCore<T>>,
    _not_sync: PhantomData<Cell<()>>,
}

struct SpscConsumer<T> {
    core: Arc<SpscCore<T>>,
    _not_sync: PhantomData<Cell<()>>,
}

fn spsc_ring<T: Send>(capacity: usize) -> (SpscProducer<T>, SpscConsumer<T>) {
    let slots = (0..capacity.saturating_add(1).max(2))
        .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let core = Arc::new(SpscCore {
        slots,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (
        SpscProducer {
            core: Arc::clone(&core),
            _not_sync: PhantomData,
        },
        SpscConsumer {
            core,
            _not_sync: PhantomData,
        },
    )
}

impl<T> SpscProducer<T> {
    fn push(&mut self, value: T) -> Result<(), T> {
        let tail = self.core.tail.load(Ordering::Relaxed);
        let next = (tail + 1) % self.core.slots.len();
        if next == self.core.head.load(Ordering::Acquire) {
            return Err(value);
        }
        // SAFETY: only this producer can own the unpublished `tail` slot.
        unsafe { (*self.core.slots[tail].get()).write(value) };
        self.core.tail.store(next, Ordering::Release);
        Ok(())
    }
}

impl<T> SpscConsumer<T> {
    fn pop(&mut self) -> Option<T> {
        let head = self.core.head.load(Ordering::Relaxed);
        if head == self.core.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: the producer published this slot with Release and cannot
        // reuse it until this consumer advances `head`.
        let value = unsafe { (*self.core.slots[head].get()).assume_init_read() };
        self.core
            .head
            .store((head + 1) % self.core.slots.len(), Ordering::Release);
        Some(value)
    }
}

struct ProtocolGroupPort {
    rx_ready: SpscConsumer<RxCompletion>,
    rx_recycle: SpscProducer<DmaBuffer>,
    tx_ready: SpscProducer<DmaBuffer>,
    tx_free: SpscConsumer<DmaBuffer>,
    pending_recycle: Vec<DmaBuffer>,
    tx_spares: Vec<DmaBuffer>,
    shared: Arc<PollGroupState>,
}

impl ProtocolGroupPort {
    fn flush_recycle(&mut self) {
        while let Some(buffer) = self.pending_recycle.pop() {
            if let Err(buffer) = self.rx_recycle.push(buffer) {
                self.pending_recycle.push(buffer);
                break;
            }
            self.shared.schedule_task();
        }
    }

    fn recycle_rx_buffer(&mut self, buffer: DmaBuffer) {
        if let Err(buffer) = self.rx_recycle.push(buffer) {
            self.pending_recycle.push(buffer);
        }
        self.shared.schedule_task();
    }

    fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame> {
        self.flush_recycle();
        let completion = self.rx_ready.pop().ok_or(NetDeviceError::Again)?;
        let frame = if completion.packet_len > completion.buffer.capacity() {
            Err(NetDeviceError::Io)
        } else {
            completion
                .buffer
                .read_with_cpu(completion.packet_len, |packet| {
                    ProtocolEthernetFrame::copy_from_slice(packet)
                })
        };
        self.recycle_rx_buffer(completion.buffer);
        frame
    }

    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult {
        let Some(mut buffer) = self.tx_spares.pop().or_else(|| self.tx_free.pop()) else {
            return Err(NetDeviceError::Again);
        };
        if buffer.set_len(frame.packet_len()).is_err() {
            self.tx_spares.push(buffer);
            return Err(NetDeviceError::InvalidParam);
        }
        buffer.write_with_cpu(|target| target.copy_from_slice(frame.packet()));
        if let Err(buffer) = self.tx_ready.push(buffer) {
            self.tx_spares.push(buffer);
            return Err(NetDeviceError::Again);
        }
        self.shared.schedule_task();
        Ok(())
    }
}

struct QueueFramePort {
    name: String,
    mac: Arc<SpinLock<[u8; 6]>>,
    groups: Vec<ProtocolGroupPort>,
    next_rx: usize,
    next_tx: usize,
}

impl EthernetFramePort for QueueFramePort {
    fn device_name(&self) -> &str {
        &self.name
    }

    fn mac_address(&self) -> [u8; 6] {
        *self.mac.lock_irqsave()
    }

    fn transmit(&mut self, frame: &ProtocolEthernetFrame) -> NetDeviceResult {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }
        for offset in 0..self.groups.len() {
            let index = (self.next_tx + offset) % self.groups.len();
            match self.groups[index].transmit(frame) {
                Ok(()) => {
                    self.next_tx = (index + 1) % self.groups.len();
                    return Ok(());
                }
                Err(NetDeviceError::Again) => {}
                Err(error) => return Err(error),
            }
        }
        Err(NetDeviceError::Again)
    }

    fn receive(&mut self) -> NetDeviceResult<ProtocolEthernetFrame> {
        if self.groups.is_empty() {
            return Err(NetDeviceError::Stopped);
        }
        for offset in 0..self.groups.len() {
            let index = (self.next_rx + offset) % self.groups.len();
            match self.groups[index].receive() {
                Ok(frame) => {
                    self.next_rx = (index + 1) % self.groups.len();
                    return Ok(frame);
                }
                Err(NetDeviceError::Again) => {}
                Err(error) => return Err(error),
            }
        }
        Err(NetDeviceError::Again)
    }
}

enum GroupPollOutcome {
    Idle(usize),
    More(usize),
    Blocked(usize),
    Failed,
}

const fn hardware_retry_outcome(work: usize) -> GroupPollOutcome {
    // The driver returned the token because only a future device event can
    // make progress.  Rearm the IRQ before sleeping instead of spinning on
    // the same token with the source masked.
    GroupPollOutcome::Idle(work)
}

const fn waits_for_hardware_event(reason: &NetError) -> bool {
    matches!(reason, NetError::Retry | NetError::LinkDown)
}

const fn rx_refill_retry_outcome(work: usize, received: usize) -> GroupPollOutcome {
    if received == 0 {
        hardware_retry_outcome(work)
    } else {
        // Reclaiming an RX descriptor can make the retained refill token
        // immediately submittable, so retry only after observable progress.
        GroupPollOutcome::More(work)
    }
}

struct QueueGroupExecutor {
    group: PreparedNetPollGroup,
    rx_ready: SpscProducer<RxCompletion>,
    rx_recycle: SpscConsumer<DmaBuffer>,
    tx_ready: SpscConsumer<DmaBuffer>,
    tx_free: SpscProducer<DmaBuffer>,
    pending_rx: Option<RxCompletion>,
    pending_rx_recycle: Option<DmaBuffer>,
    pending_tx: Option<DmaBuffer>,
    pending_tx_free: Option<DmaBuffer>,
    shared: Arc<PollGroupState>,
}

impl QueueGroupExecutor {
    fn initialize(&mut self) -> Result<(), NetError> {
        let tx_capacity = self.group.tx.capacity();
        for _ in 0..tx_capacity {
            let buffer = self
                .group
                .tx_pool
                .allocate(self.group.tx_pool.buffer_size())?;
            self.tx_free
                .push(buffer)
                .map_err(|_| NetError::InvalidParts)?;
        }
        self.group.rx.initial_refill(self.group.rx.capacity())?;
        let pending = matches!(
            self.group.irq_control.rearm_and_check()?,
            NetRearmResult::WorkPending(_)
        );
        self.shared.activate(pending);
        Ok(())
    }

    fn poll(&mut self, cpu_budget: usize) -> GroupPollOutcome {
        if self.shared.is_disabled() {
            return GroupPollOutcome::Failed;
        }
        if self.group.irq_control.quiesce().is_err() {
            self.shared.disable();
            return GroupPollOutcome::Failed;
        }

        let mut work = 0;
        if let Some(completion) = self.pending_rx.take() {
            match self.rx_ready.push(completion) {
                Ok(()) => crate::request_poll(),
                Err(completion) => {
                    self.pending_rx = Some(completion);
                    return GroupPollOutcome::Blocked(work);
                }
            }
        }
        if let Some(buffer) = self.pending_tx_free.take() {
            if let Err(buffer) = self.tx_free.push(buffer) {
                self.pending_tx_free = Some(buffer);
                return GroupPollOutcome::Blocked(work);
            }
            crate::request_poll();
        }
        let mut rx_refill_blocked = false;
        if let Some(buffer) = self.pending_rx_recycle.take() {
            match self.group.rx.recycle(buffer) {
                Ok(()) => work += 1,
                Err(error) => {
                    let (buffer, reason) = error.into_parts();
                    self.pending_rx_recycle = Some(buffer);
                    if !matches!(reason, NetError::Retry) {
                        self.shared.disable();
                        return GroupPollOutcome::Failed;
                    }
                    rx_refill_blocked = true;
                }
            }
        }

        let per_class = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut recycled = 0;
        while !rx_refill_blocked && recycled < per_class {
            let Some(buffer) = self.rx_recycle.pop() else {
                break;
            };
            match self.group.rx.recycle(buffer) {
                Ok(()) => {
                    recycled += 1;
                    work += 1;
                }
                Err(error) => {
                    let (buffer, reason) = error.into_parts();
                    self.pending_rx_recycle = Some(buffer);
                    if !matches!(reason, NetError::Retry) {
                        self.shared.disable();
                        return GroupPollOutcome::Failed;
                    }
                    rx_refill_blocked = true;
                }
            }
        }

        let rx_budget = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut received = 0;
        while received < rx_budget {
            let Some(completion) = self.group.rx.reclaim() else {
                break;
            };
            received += 1;
            work += 1;
            if let Err(completion) = self.rx_ready.push(completion) {
                self.pending_rx = Some(completion);
                return GroupPollOutcome::Blocked(work);
            }
            crate::request_poll();
        }

        let tx_completion_budget = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut tx_completed = 0;
        while tx_completed < tx_completion_budget {
            let Some(buffer) = self.group.tx.reclaim() else {
                break;
            };
            tx_completed += 1;
            work += 1;
            if let Err(buffer) = self.tx_free.push(buffer) {
                self.pending_tx_free = Some(buffer);
                return GroupPollOutcome::Blocked(work);
            }
            crate::request_poll();
        }

        let tx_submit_budget = QUEUE_BUDGET.min(cpu_budget.saturating_sub(work));
        let mut submitted = 0;
        while submitted < tx_submit_budget {
            let buffer = match self.pending_tx.take().or_else(|| self.tx_ready.pop()) {
                Some(buffer) => buffer,
                None => break,
            };
            match self.group.tx.submit(buffer) {
                Ok(()) => {
                    submitted += 1;
                    work += 1;
                }
                Err(error) => {
                    let (buffer, reason) = error.into_parts();
                    if waits_for_hardware_event(&reason) {
                        self.pending_tx = Some(buffer);
                        return hardware_retry_outcome(work);
                    }
                    if let Err(buffer) = self.tx_free.push(buffer) {
                        self.pending_tx_free = Some(buffer);
                        return GroupPollOutcome::Blocked(work);
                    }
                }
            }
        }

        if rx_refill_blocked {
            return rx_refill_retry_outcome(work, received);
        }

        let exhausted = budget_was_exhausted(received, rx_budget)
            || budget_was_exhausted(tx_completed, tx_completion_budget)
            || budget_was_exhausted(submitted, tx_submit_budget)
            || budget_was_exhausted(recycled, per_class);
        if exhausted || work >= cpu_budget {
            self.shared
                .stats
                .budget_exhaustion
                .fetch_add(1, Ordering::Relaxed);
            GroupPollOutcome::More(work)
        } else {
            GroupPollOutcome::Idle(work)
        }
    }

    fn finish_idle(&mut self) {
        if !self.shared.begin_rearm() {
            return;
        }
        match self.group.irq_control.rearm_and_check() {
            Ok(NetRearmResult::Idle) => {}
            Ok(NetRearmResult::WorkPending(_)) => {
                self.shared.stats.rearm_race.fetch_add(1, Ordering::Relaxed);
                self.shared.schedule_task();
            }
            Err(_) => self.shared.disable(),
        }
    }
}

const fn budget_was_exhausted(processed: usize, budget: usize) -> bool {
    budget != 0 && processed == budget
}

struct ExecutorControl {
    owner_cpu: usize,
    command: AtomicU8,
    affinity_status: AtomicU8,
    startup_status: AtomicU8,
    notify: Arc<ax_task::IrqNotify>,
}

struct ExecutorLease {
    control: Arc<ExecutorControl>,
    task: ax_task::AxTaskRef,
}

impl ExecutorLease {
    fn stop(&self) {
        self.control.command.store(COMMAND_STOP, Ordering::Release);
        self.control.notify.notify();
    }
}

struct WifiExecutorSlot {
    group_index: usize,
    control: Box<dyn rd_net::WifiControl>,
    queue: Arc<WifiControlQueue>,
}

fn process_wifi_requests(groups: &mut [QueueGroupExecutor], wifi: &mut [WifiExecutorSlot]) -> bool {
    let mut handled = false;
    for slot in wifi {
        while let Some(request) = slot.queue.try_pop() {
            handled = true;
            let group = &mut groups[slot.group_index];
            let mut result = group.group.irq_control.quiesce();
            if result.is_ok() {
                result = slot.control.execute(request.transaction.operation());
            }

            match group.group.irq_control.rearm_and_check() {
                Ok(NetRearmResult::Idle) => {}
                Ok(NetRearmResult::WorkPending(_)) => group.shared.schedule_task(),
                Err(error) => {
                    group.shared.disable();
                    if result.is_ok() {
                        result = Err(error);
                    }
                }
            }
            request.completion.complete(result);
        }
    }
    handled
}

fn queue_executor_main(
    mut groups: Vec<QueueGroupExecutor>,
    mut wifi: Vec<WifiExecutorSlot>,
    control: Arc<ExecutorControl>,
) {
    let affinity = ax_task::AxCpuMask::one_shot(control.owner_cpu);
    if !ax_task::set_current_affinity(affinity) {
        control
            .affinity_status
            .store(STATUS_FAILED, Ordering::Release);
        control.notify.notify();
        return;
    }
    ax_task::yield_now();
    if ax_hal::percpu::this_cpu_id() != control.owner_cpu {
        control
            .affinity_status
            .store(STATUS_FAILED, Ordering::Release);
        control.notify.notify();
        return;
    }
    control
        .affinity_status
        .store(STATUS_READY, Ordering::Release);
    control.notify.notify();

    while control.command.load(Ordering::Acquire) == COMMAND_WAIT {
        control.notify.wait();
    }
    if control.command.load(Ordering::Acquire) == COMMAND_STOP {
        return;
    }

    let initialized = groups.iter_mut().all(|group| group.initialize().is_ok());
    control.startup_status.store(
        if initialized {
            STATUS_READY
        } else {
            STATUS_FAILED
        },
        Ordering::Release,
    );
    control.notify.notify();
    if !initialized {
        for group in &mut groups {
            let _ = group.group.irq_control.quiesce();
            group.shared.disable();
        }
        return;
    }

    loop {
        if control.command.load(Ordering::Acquire) == COMMAND_STOP {
            for group in &mut groups {
                let _ = group.group.irq_control.quiesce();
                group.shared.disable();
            }
            return;
        }

        let mut cpu_work = 0;
        let mut runnable = process_wifi_requests(&mut groups, &mut wifi);
        for group in &mut groups {
            if cpu_work >= CPU_ROUND_BUDGET || !group.shared.claim() {
                continue;
            }
            runnable = true;
            match group.poll(CPU_ROUND_BUDGET - cpu_work) {
                GroupPollOutcome::Idle(work) => {
                    cpu_work += work;
                    group.finish_idle();
                }
                GroupPollOutcome::More(work) => {
                    cpu_work += work;
                    group.shared.finish_more();
                }
                GroupPollOutcome::Blocked(work) => {
                    cpu_work += work;
                }
                GroupPollOutcome::Failed => {}
            }
        }
        if runnable && cpu_work >= CPU_ROUND_BUDGET {
            ax_task::yield_now();
            continue;
        }
        if !wifi.iter().any(|slot| slot.queue.has_pending())
            && !groups.iter().any(|group| {
                let state = group.shared.state.load(Ordering::Acquire);
                state & STATE_MASK == STATE_SCHEDULED
                    || (state & STATE_MASK == STATE_POLLING && state & STATE_MISSED != 0)
            })
        {
            control.notify.wait();
        }
    }
}

struct EndpointToRegister {
    name: String,
    irq: IrqId,
    owner_cpu: usize,
    endpoint: NetHardIrqEndpoint,
    shared: Arc<PollGroupState>,
}

/// Live queue runtime.  Dropping it masks IRQs before stopping executors.
pub struct NetworkQueueRuntime {
    registrations: Vec<Box<dyn PinnedNetIrqRegistration>>,
    executors: Vec<ExecutorLease>,
    group_states: Vec<Arc<PollGroupState>>,
    _controls: Vec<Box<dyn rd_net::NetControlEndpoint>>,
    wifi_handles: Vec<WifiRuntimeHandle>,
    initial_wifi_policies: Vec<(usize, WifiLinkPolicy)>,
    protocol_owner_cpu: usize,
}

impl NetworkQueueRuntime {
    pub fn protocol_owner_cpu(&self) -> usize {
        self.protocol_owner_cpu
    }

    pub fn stats(&self) -> Vec<NetQueueStats> {
        self.group_states
            .iter()
            .map(|state| state.stats.snapshot(state.owner_cpu))
            .collect()
    }

    pub(crate) fn wifi_handle(&self, device_index: usize) -> Option<WifiRuntimeHandle> {
        self.wifi_handles
            .iter()
            .find(|handle| handle.device_index() == device_index)
            .cloned()
    }

    pub(crate) fn initial_wifi_policy(&self, device_index: usize) -> Option<WifiLinkPolicy> {
        self.initial_wifi_policies
            .iter()
            .find_map(|(index, policy)| (*index == device_index).then_some(*policy))
    }
}

impl Drop for NetworkQueueRuntime {
    fn drop(&mut self) {
        for handle in self.wifi_handles.iter().rev() {
            handle.queue.stop();
            handle.notify.notify();
        }
        disable_registrations(&self.registrations);
        stop_executors(&self.executors);
    }
}

/// Builder for an all-at-once network queue runtime.
pub struct NetworkRuntimeBuilder<'a> {
    devices: Vec<NetworkDeviceInput>,
    registrar: &'a dyn PinnedNetIrqRegistrar,
    online_cpus: usize,
}

impl<'a> NetworkRuntimeBuilder<'a> {
    pub fn new(
        devices: Vec<NetworkDeviceInput>,
        registrar: &'a dyn PinnedNetIrqRegistrar,
        online_cpus: usize,
    ) -> Self {
        Self {
            devices,
            registrar,
            online_cpus,
        }
    }

    pub fn build(
        self,
    ) -> Result<(NetworkQueueRuntime, EthernetFramePortList), NetworkRuntimeError> {
        if self.online_cpus == 0 {
            return Err(NetworkRuntimeError::InvalidTopology);
        }

        let group_irq_sets = validate_and_collect_irq_sets(&self.devices)?;
        let group_owners = assign_affinity_domains(&group_irq_sets, self.online_cpus);
        let mut groups_by_cpu = (0..self.online_cpus)
            .map(|_| Vec::new())
            .collect::<Vec<Vec<QueueGroupExecutor>>>();
        let mut wifi_by_cpu = (0..self.online_cpus)
            .map(|_| Vec::new())
            .collect::<Vec<Vec<WifiExecutorSlot>>>();
        let cpu_notifies = (0..self.online_cpus)
            .map(|_| Arc::new(ax_task::IrqNotify::new()))
            .collect::<Vec<_>>();
        let mut endpoints = Vec::new();
        let mut ports = Vec::with_capacity(self.devices.len());
        let mut controls = Vec::new();
        let mut port_macs = Vec::new();
        let mut wifi_handles = Vec::new();
        let mut startup_transactions = Vec::new();
        let mut group_states = Vec::new();
        let mut flat_group = 0;

        for (device_index, input) in self.devices.into_iter().enumerate() {
            let port_name = input.name.clone();
            let PreparedNetDevice {
                info,
                control,
                wifi_control,
                poll_groups,
            } = input.device;
            let mut protocol_groups = Vec::with_capacity(poll_groups.len());
            let mut wifi_target = None;
            for mut group in poll_groups {
                let owner_cpu = group_owners[flat_group];
                let owner_group_index = groups_by_cpu[owner_cpu].len();
                let shared = Arc::new(PollGroupState::new(
                    owner_cpu,
                    Arc::clone(&cpu_notifies[owner_cpu]),
                ));
                let (rx_ready_tx, rx_ready_rx) = spsc_ring(group.rx.capacity());
                let (rx_recycle_tx, rx_recycle_rx) = spsc_ring(group.rx.capacity());
                let (tx_ready_tx, tx_ready_rx) = spsc_ring(group.tx.capacity());
                let (tx_free_tx, tx_free_rx) = spsc_ring(group.tx.capacity());

                let resolved = &group_irq_sets[flat_group];
                for endpoint in group.irq_endpoints.drain(..) {
                    let irq = resolve_endpoint_irq(&input.irq_sources, endpoint.source_id())?;
                    if !resolved.contains(&irq) {
                        return Err(NetworkRuntimeError::InvalidTopology);
                    }
                    endpoints.push(EndpointToRegister {
                        name: format!(
                            "{}-g{}-s{}",
                            input.name,
                            group.id.get(),
                            endpoint.source_id().get()
                        ),
                        irq,
                        owner_cpu,
                        endpoint,
                        shared: Arc::clone(&shared),
                    });
                }

                protocol_groups.push(ProtocolGroupPort {
                    rx_ready: rx_ready_rx,
                    rx_recycle: rx_recycle_tx,
                    tx_ready: tx_ready_tx,
                    tx_free: tx_free_rx,
                    pending_recycle: Vec::with_capacity(group.rx.capacity()),
                    tx_spares: Vec::with_capacity(group.tx.capacity()),
                    shared: Arc::clone(&shared),
                });
                groups_by_cpu[owner_cpu].push(QueueGroupExecutor {
                    group,
                    rx_ready: rx_ready_tx,
                    rx_recycle: rx_recycle_rx,
                    tx_ready: tx_ready_rx,
                    tx_free: tx_free_tx,
                    pending_rx: None,
                    pending_rx_recycle: None,
                    pending_tx: None,
                    pending_tx_free: None,
                    shared: Arc::clone(&shared),
                });
                wifi_target.get_or_insert((owner_cpu, owner_group_index));
                group_states.push(shared);
                flat_group += 1;
            }
            if let Some(wifi_control) = wifi_control {
                let (owner_cpu, group_index) =
                    wifi_target.ok_or(NetworkRuntimeError::InvalidTopology)?;
                let queue = Arc::new(WifiControlQueue::new());
                let handle = WifiRuntimeHandle {
                    device_index,
                    owner_cpu,
                    queue: Arc::clone(&queue),
                    notify: Arc::clone(&cpu_notifies[owner_cpu]),
                };
                if let Some(transaction) = wifi_control.startup_transaction() {
                    startup_transactions.push((handle.clone(), transaction));
                }
                wifi_by_cpu[owner_cpu].push(WifiExecutorSlot {
                    group_index,
                    control: wifi_control,
                    queue,
                });
                wifi_handles.push(handle);
            }
            controls.push(control);
            let port_mac = Arc::new(SpinLock::new(info.mac_address));
            port_macs.push(Arc::clone(&port_mac));
            ports.push(Box::new(QueueFramePort {
                name: port_name,
                mac: port_mac,
                groups: protocol_groups,
                next_rx: 0,
                next_tx: 0,
            }) as Box<dyn EthernetFramePort>);
        }

        let mut executors = Vec::new();
        for (owner_cpu, (groups, wifi)) in groups_by_cpu.into_iter().zip(wifi_by_cpu).enumerate() {
            if groups.is_empty() {
                continue;
            }
            let control = Arc::new(ExecutorControl {
                owner_cpu,
                command: AtomicU8::new(COMMAND_WAIT),
                affinity_status: AtomicU8::new(STATUS_PENDING),
                startup_status: AtomicU8::new(STATUS_PENDING),
                notify: Arc::clone(&cpu_notifies[owner_cpu]),
            });
            let task_control = Arc::clone(&control);
            let task = ax_task::spawn_with_name(
                move || queue_executor_main(groups, wifi, task_control),
                format!("net-queue-cpu{owner_cpu}"),
            );
            executors.push(ExecutorLease { control, task });
        }
        for executor in &executors {
            wait_status(&executor.control.affinity_status);
            if executor.control.affinity_status.load(Ordering::Acquire) != STATUS_READY {
                stop_executors(&executors);
                return Err(NetworkRuntimeError::WorkerAffinity(
                    executor.control.owner_cpu,
                ));
            }
        }

        let mut registrations = Vec::new();
        for mut endpoint in endpoints {
            let shared = Arc::clone(&endpoint.shared);
            let owner_cpu = endpoint.owner_cpu;
            let action = PinnedNetIrqAction::new(move || match endpoint.endpoint.handle_irq() {
                NetHardIrqResult::Spurious => {
                    shared.stats.spurious.fetch_add(1, Ordering::Relaxed);
                    PinnedNetIrqOutcome::Unhandled
                }
                NetHardIrqResult::Schedule(_snapshot) => {
                    shared.schedule_irq();
                    PinnedNetIrqOutcome::Wake
                }
                NetHardIrqResult::ProbeDeferred => {
                    shared.stats.probe_deferred.fetch_add(1, Ordering::Relaxed);
                    shared.schedule_irq();
                    PinnedNetIrqOutcome::Wake
                }
            });
            let registration =
                match self
                    .registrar
                    .register(endpoint.name, endpoint.irq, owner_cpu, action)
                {
                    Ok(registration) if registration.owner_cpu() == owner_cpu => registration,
                    Ok(registration) => {
                        let _ = registration.disable_and_synchronize();
                        disable_registrations(&registrations);
                        stop_executors(&executors);
                        return Err(NetworkRuntimeError::InvalidTopology);
                    }
                    Err(error) => {
                        disable_registrations(&registrations);
                        stop_executors(&executors);
                        return Err(error.into());
                    }
                };
            registrations.push(registration);
        }

        for executor in &executors {
            executor
                .control
                .command
                .store(COMMAND_START, Ordering::Release);
            executor.control.notify.notify();
        }
        for executor in &executors {
            wait_status(&executor.control.startup_status);
            if executor.control.startup_status.load(Ordering::Acquire) != STATUS_READY {
                disable_registrations(&registrations);
                stop_executors(&executors);
                return Err(NetworkRuntimeError::QueueInit);
            }
        }
        for registration in &registrations {
            if let Err(error) = registration.enable() {
                disable_registrations(&registrations);
                stop_executors(&executors);
                return Err(error.into());
            }
        }

        let protocol_owner_cpu = select_protocol_owner(&group_owners, self.online_cpus);
        let mut runtime = NetworkQueueRuntime {
            registrations,
            executors,
            group_states,
            _controls: controls,
            wifi_handles,
            initial_wifi_policies: Vec::new(),
            protocol_owner_cpu,
        };
        for (handle, transaction) in startup_transactions {
            let policy = transaction.link_policy();
            handle.submit(transaction)?;
            if let Some(policy) = policy {
                runtime
                    .initial_wifi_policies
                    .push((handle.device_index(), policy));
            }
        }
        for (control, mac) in runtime._controls.iter_mut().zip(port_macs) {
            let address = control.mac_address()?;
            *mac.lock_irqsave() = address;
        }
        Ok((runtime, ports))
    }
}

fn validate_and_collect_irq_sets(
    devices: &[NetworkDeviceInput],
) -> Result<Vec<Vec<IrqId>>, NetworkRuntimeError> {
    let mut sets = Vec::new();
    for input in devices {
        if input.device.poll_groups.is_empty() || input.irq_sources.is_empty() {
            return Err(NetworkRuntimeError::InvalidTopology);
        }
        for group in &input.device.poll_groups {
            if group.irq_endpoints.is_empty() {
                return Err(NetworkRuntimeError::InvalidTopology);
            }
            let mut irqs = Vec::new();
            for endpoint in &group.irq_endpoints {
                let irq = resolve_endpoint_irq(&input.irq_sources, endpoint.source_id())?;
                if !irqs.contains(&irq) {
                    irqs.push(irq);
                }
            }
            sets.push(irqs);
        }
        for source in &input.irq_sources {
            let used = input.device.poll_groups.iter().any(|group| {
                group
                    .irq_endpoints
                    .iter()
                    .any(|endpoint| endpoint.source_id() == source.source_id)
            });
            if !used {
                return Err(NetworkRuntimeError::InvalidTopology);
            }
        }
    }
    Ok(sets)
}

fn resolve_endpoint_irq(
    sources: &[ResolvedNetIrqSource],
    source_id: NetIrqSourceId,
) -> Result<IrqId, NetworkRuntimeError> {
    let mut matches = sources
        .iter()
        .filter(|source| source.source_id == source_id)
        .map(|source| source.irq);
    let irq = matches.next().ok_or(NetworkRuntimeError::InvalidTopology)?;
    if matches.next().is_some() {
        return Err(NetworkRuntimeError::InvalidTopology);
    }
    Ok(irq)
}

fn assign_affinity_domains(irq_sets: &[Vec<IrqId>], cpu_count: usize) -> Vec<usize> {
    let mut parents = (0..irq_sets.len()).collect::<Vec<_>>();
    for left in 0..irq_sets.len() {
        for right in (left + 1)..irq_sets.len() {
            if irq_sets[left]
                .iter()
                .any(|irq| irq_sets[right].contains(irq))
            {
                union(&mut parents, left, right);
            }
        }
    }
    let mut roots = Vec::new();
    let mut owners = Vec::with_capacity(irq_sets.len());
    for index in 0..irq_sets.len() {
        let root = find(&mut parents, index);
        let domain_index = match roots.iter().position(|candidate| *candidate == root) {
            Some(index) => index,
            None => {
                roots.push(root);
                roots.len() - 1
            }
        };
        owners.push(domain_index % cpu_count);
    }
    owners
}

fn find(parents: &mut [usize], mut index: usize) -> usize {
    while parents[index] != index {
        let grandparent = parents[parents[index]];
        parents[index] = grandparent;
        index = grandparent;
    }
    index
}

fn union(parents: &mut [usize], left: usize, right: usize) {
    let left_root = find(parents, left);
    let right_root = find(parents, right);
    if left_root != right_root {
        let (first, second) = if left_root < right_root {
            (left_root, right_root)
        } else {
            (right_root, left_root)
        };
        parents[second] = first;
    }
}

fn select_protocol_owner(group_owners: &[usize], cpu_count: usize) -> usize {
    let mut load = vec![0usize; cpu_count];
    for &owner in group_owners {
        load[owner] += 1;
    }
    load.iter()
        .enumerate()
        .min_by_key(|(cpu, groups)| (**groups, *cpu))
        .map(|(cpu, _)| cpu)
        .unwrap_or(0)
}

fn wait_status(status: &AtomicU8) {
    while status.load(Ordering::Acquire) == STATUS_PENDING {
        ax_task::yield_now();
    }
}

fn disable_registrations(registrations: &[Box<dyn PinnedNetIrqRegistration>]) {
    for registration in registrations.iter().rev() {
        let _ = registration.disable_and_synchronize();
    }
}

fn stop_executors(executors: &[ExecutorLease]) {
    for executor in executors.iter().rev() {
        executor.stop();
    }
    for executor in executors.iter().rev() {
        executor.task.join();
    }
}

#[cfg(test)]
mod tests {
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
    use std::{
        alloc::{alloc_zeroed, dealloc},
        sync::Mutex as StdMutex,
    };

    use irq_framework::{HwIrq, IrqDomainId};
    use rd_net::dma_api::{
        DeviceDma, DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDirection,
        DmaDomainId, DmaError, DmaMapHandle, DmaOp,
    };

    use super::*;

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    struct TestDma;

    impl TestDma {
        unsafe fn allocate(layout: Layout) -> Option<DmaAllocHandle> {
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            Some(unsafe {
                DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as usize as u64).into(), layout)
            })
        }
    }

    impl DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            unsafe { Self::allocate(layout) }
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn alloc_coherent(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            unsafe { Self::allocate(layout) }
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            let layout = Layout::from_size_align(size.get(), 1)?;
            Ok(unsafe {
                DmaMapHandle::new(addr, (addr.as_ptr() as usize as u64).into(), layout, None)
            })
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    static TEST_DMA: TestDma = TestDma;

    fn dma_buffer(capacity: usize, len: usize) -> DmaBuffer {
        let device = DeviceDma::new(
            DmaDeviceInfo::new(
                DmaDomainId::Direct,
                DmaCoherency::Coherent,
                DmaConstraints::new(u64::MAX),
            ),
            &TEST_DMA,
        );
        let pool = device.contiguous_buffer_pool(
            Layout::from_size_align(capacity, 64).unwrap(),
            DmaDirection::Bidirectional,
            1,
        );
        DmaBuffer::new(pool.alloc().unwrap(), len)
            .unwrap_or_else(|_| panic!("test DMA token length exceeds its allocation"))
    }

    struct RecordingRegistration {
        id: usize,
        order: Arc<StdMutex<Vec<usize>>>,
    }

    impl PinnedNetIrqRegistration for RecordingRegistration {
        fn owner_cpu(&self) -> usize {
            0
        }

        fn enable(&self) -> Result<(), PinnedNetIrqError> {
            Ok(())
        }

        fn disable_and_synchronize(&self) -> Result<(), PinnedNetIrqError> {
            self.order.lock().unwrap().push(self.id);
            Ok(())
        }
    }

    #[derive(Clone, Copy)]
    enum ModelOperation {
        Publish,
        Rearm,
        FinishMore,
        Disable,
    }

    fn irq(line: u32) -> IrqId {
        IrqId::new(IrqDomainId(1), HwIrq(line))
    }

    #[test]
    fn spsc_ring_is_bounded_and_preserves_move_order() {
        let (mut producer, mut consumer) = spsc_ring(2);
        assert!(producer.push(10).is_ok());
        assert!(producer.push(20).is_ok());
        assert_eq!(producer.push(30), Err(30));
        assert_eq!(consumer.pop(), Some(10));
        assert_eq!(consumer.pop(), Some(20));
        assert_eq!(consumer.pop(), None);
    }

    #[test]
    fn failed_initialization_unwinds_irq_leases_in_reverse_order() {
        let order = Arc::new(StdMutex::new(Vec::new()));
        let registrations = (0..3)
            .map(|id| {
                Box::new(RecordingRegistration {
                    id,
                    order: Arc::clone(&order),
                }) as Box<dyn PinnedNetIrqRegistration>
            })
            .collect::<Vec<_>>();

        disable_registrations(&registrations);
        assert_eq!(*order.lock().unwrap(), vec![2, 1, 0]);
    }

    #[test]
    fn spsc_ring_drops_each_move_only_token_exactly_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let (mut producer, consumer) = spsc_ring(1);
        assert!(producer.push(DropProbe(Arc::clone(&drops))).is_ok());
        let rejected = match producer.push(DropProbe(Arc::clone(&drops))) {
            Err(token) => token,
            Ok(()) => panic!("full ring must return ownership to the producer"),
        };
        drop(rejected);
        assert_eq!(drops.load(Ordering::Relaxed), 1);

        drop(producer);
        drop(consumer);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn oversized_rx_frame_recycles_token_and_next_frame_remains_receivable() {
        let (mut rx_ready_tx, rx_ready_rx) = spsc_ring(2);
        let (mut rx_recycle_tx, mut rx_recycle_rx) = spsc_ring(1);
        let (tx_ready_tx, _tx_ready_rx) = spsc_ring(1);
        let (_tx_free_tx, tx_free_rx) = spsc_ring(1);
        let oversized = dma_buffer(4096, 4096);
        let oversized_bus_addr = oversized.bus_addr();
        let valid = dma_buffer(4096, 64);
        let valid_bus_addr = valid.bus_addr();
        let occupied = dma_buffer(4096, 64);
        let occupied_bus_addr = occupied.bus_addr();

        rx_ready_tx
            .push(RxCompletion {
                buffer: oversized,
                packet_len: 2049,
            })
            .unwrap();
        rx_ready_tx
            .push(RxCompletion {
                buffer: valid,
                packet_len: 64,
            })
            .unwrap();
        rx_recycle_tx.push(occupied).unwrap();

        let shared = Arc::new(group_state(STATE_IDLE));
        let mut port = ProtocolGroupPort {
            rx_ready: rx_ready_rx,
            rx_recycle: rx_recycle_tx,
            tx_ready: tx_ready_tx,
            tx_free: tx_free_rx,
            pending_recycle: Vec::with_capacity(2),
            tx_spares: Vec::new(),
            shared,
        };

        assert!(matches!(port.receive(), Err(NetDeviceError::InvalidParam)));
        assert_eq!(port.pending_recycle.len(), 1);
        assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), occupied_bus_addr);

        let frame = port
            .receive()
            .expect("a malformed frame must not consume the next completion");
        assert_eq!(frame.packet_len(), 64);
        assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), oversized_bus_addr);

        port.flush_recycle();
        assert_eq!(rx_recycle_rx.pop().unwrap().bus_addr(), valid_bus_addr);
        assert!(port.pending_recycle.is_empty());

        let invalid_length = dma_buffer(2048, 2048);
        let invalid_length_bus_addr = invalid_length.bus_addr();
        rx_ready_tx
            .push(RxCompletion {
                buffer: invalid_length,
                packet_len: 2049,
            })
            .unwrap();
        assert!(matches!(port.receive(), Err(NetDeviceError::Io)));
        assert_eq!(
            rx_recycle_rx.pop().unwrap().bus_addr(),
            invalid_length_bus_addr
        );
    }

    fn group_state(initial: u8) -> PollGroupState {
        let state = PollGroupState::new(0, Arc::new(ax_task::IrqNotify::new()));
        state.state.store(initial, Ordering::Release);
        state
    }

    fn apply_model_operation(state: &PollGroupState, operation: ModelOperation) {
        match operation {
            ModelOperation::Publish => state.schedule_task(),
            ModelOperation::Rearm => {
                let _ = state.begin_rearm();
            }
            ModelOperation::FinishMore => state.finish_more(),
            ModelOperation::Disable => state.disable(),
        }
    }

    #[test]
    fn shared_irq_groups_are_assigned_to_the_same_cpu() {
        let owners = assign_affinity_domains(&[vec![irq(4)], vec![irq(4)], vec![irq(5)]], 4);
        assert_eq!(owners[0], owners[1]);
        assert_ne!(owners[0], owners[2]);
    }

    #[test]
    fn affinity_domains_merge_transitively_through_shared_sources() {
        let owners = assign_affinity_domains(
            &[
                vec![irq(1)],
                vec![irq(1), irq(2)],
                vec![irq(2)],
                vec![irq(3)],
            ],
            4,
        );
        assert_eq!(owners[0], owners[1]);
        assert_eq!(owners[1], owners[2]);
        assert_ne!(owners[2], owners[3]);
    }

    #[test]
    fn independent_sources_can_use_different_cpus() {
        let owners = assign_affinity_domains(&[vec![irq(1)], vec![irq(2)]], 4);
        assert_eq!(owners, vec![0, 1]);
    }

    #[test]
    fn missed_event_survives_poll_completion() {
        let notify = Arc::new(ax_task::IrqNotify::new());
        let state = PollGroupState::new(0, notify);
        state.activate(false);
        state.schedule_task();
        state.state.store(STATE_POLLING, Ordering::Release);
        state.schedule_task();
        assert!(!state.begin_rearm());
        assert_eq!(
            state.state.load(Ordering::Acquire) & STATE_MASK,
            STATE_SCHEDULED
        );
    }

    #[test]
    fn rearm_window_is_linearizable_in_both_event_orders() {
        for operations in [
            [ModelOperation::Publish, ModelOperation::Rearm],
            [ModelOperation::Rearm, ModelOperation::Publish],
        ] {
            let state = group_state(STATE_POLLING);
            for operation in operations {
                apply_model_operation(&state, operation);
            }
            assert_eq!(
                state.state.load(Ordering::Acquire) & STATE_MASK,
                STATE_SCHEDULED
            );
        }
    }

    #[test]
    fn disabled_group_cannot_be_resurrected_by_any_completion_order() {
        let permutations = [
            [
                ModelOperation::Publish,
                ModelOperation::FinishMore,
                ModelOperation::Disable,
            ],
            [
                ModelOperation::Publish,
                ModelOperation::Disable,
                ModelOperation::FinishMore,
            ],
            [
                ModelOperation::FinishMore,
                ModelOperation::Publish,
                ModelOperation::Disable,
            ],
            [
                ModelOperation::FinishMore,
                ModelOperation::Disable,
                ModelOperation::Publish,
            ],
            [
                ModelOperation::Disable,
                ModelOperation::Publish,
                ModelOperation::FinishMore,
            ],
            [
                ModelOperation::Disable,
                ModelOperation::FinishMore,
                ModelOperation::Publish,
            ],
        ];

        for operations in permutations {
            let state = group_state(STATE_POLLING);
            for operation in operations {
                apply_model_operation(&state, operation);
            }
            assert_eq!(
                state.state.load(Ordering::Acquire) & STATE_MASK,
                STATE_DISABLED
            );
        }
    }

    #[test]
    fn queue_budget_only_reports_a_nonzero_exact_exhaustion() {
        assert!(!budget_was_exhausted(0, 0));
        assert!(!budget_was_exhausted(63, 64));
        assert!(budget_was_exhausted(64, 64));
    }

    #[test]
    fn hardware_retry_rearms_instead_of_immediately_rescheduling() {
        assert!(matches!(
            hardware_retry_outcome(0),
            GroupPollOutcome::Idle(0)
        ));
        assert!(waits_for_hardware_event(&NetError::Retry));
        assert!(waits_for_hardware_event(&NetError::LinkDown));
        assert!(!waits_for_hardware_event(&NetError::NotSupported));
        assert!(matches!(
            rx_refill_retry_outcome(0, 0),
            GroupPollOutcome::Idle(0)
        ));
        assert!(matches!(
            rx_refill_retry_outcome(1, 1),
            GroupPollOutcome::More(1)
        ));
    }

    #[test]
    fn protocol_owner_uses_the_least_loaded_cpu() {
        assert_eq!(select_protocol_owner(&[0, 0, 1], 4), 2);
        assert_eq!(select_protocol_owner(&[0, 1, 2, 3], 4), 0);
    }
}
