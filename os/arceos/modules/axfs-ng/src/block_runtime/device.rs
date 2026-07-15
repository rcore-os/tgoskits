use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};
use core::{
    mem,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use dma_api::{DeviceDma, DmaDirection, DmaDomainId};
use irq_framework::IrqId;
use rdif_block::{
    BlkError, CompletionHint, CompletionSink as RdifCompletionSink, DeviceInfo, IQueue,
    OwnedRequest, QueueHandle, QueueInfo, Request, RequestFlags, RequestId, RequestOp,
    RequestPoll as OwnedRequestPoll, RequestStatus, TransferChunk, TransferPlanner,
    TransferRuntimeCaps, validate_request,
};

use super::{
    BlockIrqBridge, CompletionDrain, CompletionSink, DmaBufferGuard, DrainEvents, PendingTable,
    PollOutcome, RequestKey, RequestPoller, RuntimeDmaBuffer, RuntimeEventLatch,
    new_owned_dma_buffer,
};
use crate::os::{
    BlockIrqOutcome, BlockIrqRegistration, current_task_id, dma_op, notify_drain,
    notify_drain_from_irq, notify_waiters, register_shared_block_irq, spawn_task,
    sync::IrqMutex as SpinNoIrq, task_can_block, task_wait_timeout, task_yield,
    wait_for_drain_notification_timeout, wake_task,
};

const DEFAULT_MAX_TRANSFER_BYTES: usize = 1024 * 1024;
const IRQ_DRIVEN_MAX_TRANSFER_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_SUBMIT_WINDOW: usize = 32;
const IRQ_COMPLETION_REPOLL_TIMEOUT: Duration = Duration::from_millis(1);
const IRQ_COMPLETION_FAST_POLL_ATTEMPTS: usize = 32;

fn runtime_device_dma(domain: DmaDomainId, dma_mask: u64) -> Result<DeviceDma, BlkError> {
    let dma_op = dma_op().ok_or(BlkError::Io)?;
    Ok(DeviceDma::new(domain, dma_mask, dma_op))
}

type CompletionRecord = (RequestId, Result<(), BlkError>, Option<RuntimeDmaBuffer>);

static BLOCK_DRAIN_DEVICE_BITS: AtomicU64 = AtomicU64::new(0);
static BLOCK_DRAIN_FULL_SCAN: AtomicBool = AtomicBool::new(false);
static BLOCK_DRAIN_SPAWNED: spin::Once<()> = spin::Once::new();
static BLOCK_RUNTIME: spin::Once<Arc<BlockRuntime>> = spin::Once::new();

const SECTOR_BYTES: u64 = 512;

static BLOCK_READS: AtomicU64 = AtomicU64::new(0);
static BLOCK_SECTORS_READ: AtomicU64 = AtomicU64::new(0);
static BLOCK_WRITES: AtomicU64 = AtomicU64::new(0);
static BLOCK_SECTORS_WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Cumulative block-device I/O counters, in the order
/// `(reads, sectors_read, writes, sectors_written)`, where a sector is 512
/// bytes. Read counters cover `RequestOp::Read` submissions and write counters
/// cover `RequestOp::Write` submissions.
pub fn block_io_stats() -> (u64, u64, u64, u64) {
    (
        BLOCK_READS.load(Ordering::Relaxed),
        BLOCK_SECTORS_READ.load(Ordering::Relaxed),
        BLOCK_WRITES.load(Ordering::Relaxed),
        BLOCK_SECTORS_WRITTEN.load(Ordering::Relaxed),
    )
}

pub fn release_block_irqs_for_passthrough() -> usize {
    BLOCK_RUNTIME.get().map_or(0, |runtime| {
        runtime.release_irq_registrations_for_passthrough()
    })
}

#[derive(Clone, Copy)]
struct WindowEntry {
    key: RequestKey,
    queue_id: usize,
    byte_offset: usize,
    byte_len: usize,
}

#[derive(Clone, Copy)]
struct ActiveQueue {
    index: usize,
    queue_id: usize,
    window: usize,
}

struct QueueProgressWaiter {
    queue_id: usize,
    task_id: u64,
}

struct BarrierWaiter {
    task_id: u64,
}

struct RuntimeDrainWake {
    device_index: usize,
}

impl BlockDrainWake for RuntimeDrainWake {
    fn wake_drain(&self) {
        mark_block_drain_device(self.device_index);
    }

    fn wake_drain_from_irq(&self) {
        mark_block_drain_device_from_irq(self.device_index);
    }
}

#[derive(Clone, Copy)]
struct DrainSelection {
    full_scan: bool,
    device_bits: u64,
}

struct DataIoGuard<'a> {
    device: &'a BlockDeviceHandle,
}

impl Drop for DataIoGuard<'_> {
    fn drop(&mut self) {
        self.device.finish_data_io();
    }
}

#[cfg(feature = "ext4")]
struct FlushBarrierGuard<'a> {
    device: &'a BlockDeviceHandle,
}

#[cfg(feature = "ext4")]
impl Drop for FlushBarrierGuard<'_> {
    fn drop(&mut self) {
        self.device.flush_active.store(false, Ordering::Release);
        self.device.wake_barrier_waiters();
    }
}

pub trait BlockDrainWake: Send + Sync {
    fn wake_drain(&self);

    fn wake_drain_from_irq(&self) {
        self.wake_drain();
    }
}

#[cfg(test)]
pub struct NoopDrainWake;

#[cfg(test)]
impl BlockDrainWake for NoopDrainWake {
    fn wake_drain(&self) {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockCompletionMode {
    Polling,
    IrqDriven,
}

impl BlockCompletionMode {
    const fn as_usize(self) -> usize {
        match self {
            Self::Polling => 0,
            Self::IrqDriven => 1,
        }
    }

    const fn from_usize(value: usize) -> Self {
        match value {
            1 => Self::IrqDriven,
            _ => Self::Polling,
        }
    }
}

pub struct BlockRuntimeConfig {
    pub drain_wake: Arc<dyn BlockDrainWake>,
    pub completion_mode: BlockCompletionMode,
    pub max_transfer_bytes: usize,
    pub max_segments: usize,
    pub submit_window: usize,
}

impl BlockRuntimeConfig {
    pub fn new(drain_wake: Arc<dyn BlockDrainWake>) -> Self {
        Self {
            drain_wake,
            completion_mode: BlockCompletionMode::Polling,
            max_transfer_bytes: DEFAULT_MAX_TRANSFER_BYTES,
            max_segments: usize::MAX,
            submit_window: DEFAULT_SUBMIT_WINDOW,
        }
    }
}

pub struct BlockDeviceHandle {
    name: String,
    queues: Box<[SpinNoIrq<QueueRuntime>]>,
    event_latch: RuntimeEventLatch,
    pending: SpinNoIrq<PendingTable>,
    queue_progress_waiters: SpinNoIrq<Vec<QueueProgressWaiter>>,
    barrier_waiters: SpinNoIrq<Vec<BarrierWaiter>>,
    drain_wake: Arc<dyn BlockDrainWake>,
    submit_window: usize,
    completion_mode: AtomicUsize,
    drain_running: AtomicBool,
    active_data_ops: AtomicUsize,
    flush_active: AtomicBool,
    poisoned: AtomicBool,
}

pub struct QueueRuntime {
    queue: RuntimeQueue,
    driver_queue_id: usize,
    info: QueueInfo,
    planner: TransferPlanner,
}

enum RuntimeQueue {
    Legacy(Box<dyn IQueue>),
    Owned(QueueHandle),
}

impl RuntimeQueue {
    fn info(&self) -> QueueInfo {
        match self {
            Self::Legacy(queue) => queue.info(),
            Self::Owned(queue) => queue.info(),
        }
    }
}

impl QueueRuntime {
    fn new(
        queue: RuntimeQueue,
        runtime_queue_id: usize,
        caps: TransferRuntimeCaps,
    ) -> Result<Self, BlkError> {
        let mut info = queue.info();
        let driver_queue_id = info.id;
        if info.limits.max_inflight == 0 {
            return Err(BlkError::InvalidRequest);
        }
        info.id = runtime_queue_id;
        let planner = TransferPlanner::new(info.device, info.limits, caps)?;
        Ok(Self {
            queue,
            driver_queue_id,
            info,
            planner,
        })
    }

    pub const fn info(&self) -> QueueInfo {
        self.info
    }
}

pub struct BlockRuntime {
    devices: Vec<Arc<BlockDeviceHandle>>,
    irq_registrations: SpinNoIrq<Vec<Box<dyn BlockIrqRegistration>>>,
}

impl BlockRuntime {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
            irq_registrations: SpinNoIrq::new(Vec::new()),
        }
    }

    pub fn push_device(&mut self, device: Arc<BlockDeviceHandle>) {
        self.devices.push(device);
    }

    pub fn devices(&self) -> &[Arc<BlockDeviceHandle>] {
        &self.devices
    }

    pub fn push_irq_registration(&self, registration: Box<dyn BlockIrqRegistration>) {
        self.irq_registrations.lock().push(registration);
    }

    pub fn release_irq_registrations_for_passthrough(&self) -> usize {
        for device in &self.devices {
            device.set_completion_mode(BlockCompletionMode::Polling);
        }

        let (released, registrations) = {
            let mut registrations = self.irq_registrations.lock();
            let released = registrations.len();
            (released, mem::take(&mut *registrations))
        };
        drop(registrations);
        released
    }

    #[cfg(test)]
    pub(crate) fn irq_registration_count(&self) -> usize {
        self.irq_registrations.lock().len()
    }
}

impl Default for BlockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub struct RdifBlockDevice {
    name: String,
    irqs: Vec<BlockIrqSource>,
    interface: Box<dyn rdif_block::Interface>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockIrqSource {
    pub source_id: usize,
    pub irq: IrqId,
}

impl RdifBlockDevice {
    pub fn new(
        name: impl Into<String>,
        irq: Option<IrqId>,
        interface: Box<dyn rdif_block::Interface>,
    ) -> Self {
        Self::new_with_irqs(
            name,
            irq.into_iter()
                .map(|irq| BlockIrqSource { source_id: 0, irq }),
            interface,
        )
    }

    pub fn new_with_irqs(
        name: impl Into<String>,
        irqs: impl IntoIterator<Item = BlockIrqSource>,
        interface: Box<dyn rdif_block::Interface>,
    ) -> Self {
        Self {
            name: name.into(),
            irqs: irqs.into_iter().collect(),
            interface,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn irq(&self) -> Option<IrqId> {
        self.irq_for_source(0)
            .or_else(|| self.irqs.first().map(|source| source.irq))
    }

    pub fn irq_for_source(&self, source_id: usize) -> Option<IrqId> {
        self.irqs
            .iter()
            .find(|source| source.source_id == source_id)
            .map(|source| source.irq)
    }

    pub fn irq_sources(&self) -> &[BlockIrqSource] {
        &self.irqs
    }

    pub fn interface(&self) -> &dyn rdif_block::Interface {
        &*self.interface
    }

    pub fn interface_mut(&mut self) -> &mut dyn rdif_block::Interface {
        &mut *self.interface
    }

    pub fn enable_irq(&self) {
        self.interface.enable_irq();
    }

    pub fn disable_irq(&self) {
        self.interface.disable_irq();
    }

    pub fn is_irq_enabled(&self) -> bool {
        self.interface.is_irq_enabled()
    }

    pub fn take_irq_handler(
        &mut self,
        source_id: usize,
    ) -> Option<(IrqId, Box<dyn rdif_block::IrqHandler>)> {
        let irq = self.irq_for_source(source_id)?;
        self.interface
            .take_irq_handler(source_id)
            .map(|handler| (irq, handler))
    }
}

pub struct BlockIrqAction {
    handler: Box<dyn rdif_block::IrqHandler>,
    device: Arc<BlockDeviceHandle>,
    device_index: usize,
}

impl BlockIrqAction {
    pub fn new(
        handler: Box<dyn rdif_block::IrqHandler>,
        device: Arc<BlockDeviceHandle>,
        device_index: usize,
    ) -> Self {
        Self {
            handler,
            device,
            device_index,
        }
    }

    pub fn run(&mut self) -> BlockIrqOutcome {
        let event = self.handler.handle_irq();
        if self.device.record_driver_event(event) {
            self.device.drain_wake.wake_drain_from_irq();
            BlockIrqOutcome::Wake
        } else {
            BlockIrqOutcome::Handled
        }
    }

    pub const fn device_index(&self) -> usize {
        self.device_index
    }
}

impl BlockRuntime {
    pub fn from_rdif_devices(devices: impl IntoIterator<Item = RdifBlockDevice>) -> Self {
        let mut runtime = Self::new();
        for block in devices {
            let device_index = runtime.devices.len();
            let drain_wake = Arc::new(RuntimeDrainWake { device_index });
            match build_rdif_block_device(block, device_index, drain_wake) {
                Ok(registered) => {
                    let (device, registrations) = registered;
                    for registration in registrations {
                        runtime.push_irq_registration(registration);
                    }
                    runtime.push_device(device);
                }
                Err(err) => warn!("failed to register rdif filesystem block device: {err:?}"),
            }
        }
        runtime
    }

    pub fn install_from_rdif_devices(
        devices: impl IntoIterator<Item = RdifBlockDevice>,
    ) -> Arc<BlockRuntime> {
        let runtime = Arc::new(Self::from_rdif_devices(devices));
        BLOCK_RUNTIME.call_once(|| runtime.clone());
        spawn_block_drain_task(runtime.clone());
        runtime
    }
}

impl BlockDeviceHandle {
    pub fn new(
        name: impl Into<String>,
        queues: impl IntoIterator<Item = Box<dyn IQueue>>,
        bridge: Arc<BlockIrqBridge>,
        config: BlockRuntimeConfig,
    ) -> Result<Arc<Self>, BlkError> {
        Self::new_runtime(
            name,
            queues.into_iter().map(RuntimeQueue::Legacy),
            bridge,
            config,
        )
    }

    fn new_runtime(
        name: impl Into<String>,
        queues: impl IntoIterator<Item = RuntimeQueue>,
        bridge: Arc<BlockIrqBridge>,
        config: BlockRuntimeConfig,
    ) -> Result<Arc<Self>, BlkError> {
        let caps = TransferRuntimeCaps::new(config.max_transfer_bytes, config.max_segments);
        let mut driver_queue_map = [None; u64::BITS as usize];
        let mut first_device = None;
        let queues = queues
            .into_iter()
            .enumerate()
            .map(|(runtime_queue_id, queue)| {
                let info = queue.info();
                let driver_queue_id = info.id;
                if driver_queue_id >= driver_queue_map.len()
                    || driver_queue_map[driver_queue_id].is_some()
                {
                    return Err(BlkError::InvalidRequest);
                }
                if let Some(device) = first_device {
                    if !same_device_identity(device, info.device) {
                        return Err(BlkError::InvalidRequest);
                    }
                } else {
                    first_device = Some(info.device);
                }
                driver_queue_map[driver_queue_id] = Some(runtime_queue_id);
                QueueRuntime::new(queue, runtime_queue_id, caps).map(SpinNoIrq::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if queues.is_empty() {
            return Err(BlkError::InvalidRequest);
        }

        Ok(Arc::new(Self {
            name: name.into(),
            queues: queues.into_boxed_slice(),
            event_latch: RuntimeEventLatch::new(bridge, driver_queue_map),
            pending: SpinNoIrq::new(PendingTable::new()),
            queue_progress_waiters: SpinNoIrq::new(Vec::new()),
            barrier_waiters: SpinNoIrq::new(Vec::new()),
            drain_wake: config.drain_wake,
            submit_window: config.submit_window.max(1),
            completion_mode: AtomicUsize::new(config.completion_mode.as_usize()),
            drain_running: AtomicBool::new(false),
            active_data_ops: AtomicUsize::new(0),
            flush_active: AtomicBool::new(false),
            poisoned: AtomicBool::new(false),
        }))
    }

    pub fn bridge(&self) -> Arc<BlockIrqBridge> {
        self.event_latch.bridge()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn completion_mode(&self) -> BlockCompletionMode {
        BlockCompletionMode::from_usize(self.completion_mode.load(Ordering::Acquire))
    }

    pub fn set_completion_mode(&self, mode: BlockCompletionMode) {
        self.completion_mode
            .store(mode.as_usize(), Ordering::Release);
    }

    pub fn queue_ids(&self) -> Vec<usize> {
        self.queues
            .iter()
            .map(|queue| queue.lock().driver_queue_id)
            .collect()
    }

    pub fn pending_queue_bits(&self) -> u64 {
        self.pending.lock().pending_queue_bits()
    }

    pub fn has_pending_requests(&self) -> bool {
        self.pending.lock().pending_queue_bits() != 0
    }

    pub fn device_info(&self) -> DeviceInfo {
        self.queues[0].lock().info.device
    }

    pub fn drain_events(&self) -> usize {
        self.with_drain(|| {
            let events = self.event_latch.bridge().take_events();
            self.drain_given_events(events)
        })
    }

    pub fn drain_hint(&self, hint: CompletionHint) -> usize {
        self.with_drain(|| {
            let mut poller = DeviceRequestPoller { device: self };
            CompletionDrain::new(&self.pending, &mut poller).drain_hint(hint)
        })
    }

    /// Re-poll *every* currently-pending request, regardless of whether a
    /// driver event was recorded for it. `drain_events` only polls requests
    /// whose completion IRQ produced a recorded bridge event; if that event
    /// was dropped a request is never re-polled and the submitter deadlocks.
    pub fn drain_all_pending(&self) -> usize {
        self.with_drain(|| {
            let keys = self.pending.lock().active_keys();
            if keys.is_empty() {
                return 0;
            }
            let mut poller = DeviceRequestPoller { device: self };
            CompletionDrain::new(&self.pending, &mut poller).poll_keys(&keys)
        })
    }

    pub fn record_driver_event(&self, event: rdif_block::Event) -> bool {
        self.event_latch.record_driver_event(event)
    }

    #[cfg(test)]
    pub(crate) fn pending_count_for_queue(&self, queue_id: usize) -> usize {
        self.pending.lock().keys_for_queue(queue_id).len()
    }

    #[cfg(test)]
    pub(crate) fn pending_queue_ready_events(&self) -> u64 {
        self.event_latch.bridge().take_events().queue_bits
    }

    fn with_drain(&self, f: impl FnOnce() -> usize) -> usize {
        if self
            .drain_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return 0;
        }
        let completed = f();
        self.drain_running.store(false, Ordering::Release);
        completed
    }

    fn drain_given_events(&self, events: DrainEvents) -> usize {
        let mut poller = DeviceRequestPoller { device: self };
        CompletionDrain::new(&self.pending, &mut poller).drain_events(events)
    }

    fn wake_drain_after_irq_submit(&self) {
        if self.completion_mode() == BlockCompletionMode::IrqDriven {
            self.drain_wake.wake_drain();
        }
    }

    #[cfg(feature = "ext4")]
    fn poll_one(&self, key: RequestKey) -> bool {
        let mut poller = DeviceRequestPoller { device: self };
        CompletionDrain::new(&self.pending, &mut poller).poll_one(key)
    }

    fn wake_completed_request(&self, key: RequestKey, task_id: Option<u64>) {
        let queue_id = self.queue_id_for_key(key);
        if let Some(queue_id) = queue_id {
            self.wake_queue_progress(queue_id);
        }
        notify_waiters();
        if let Some(task_id) = task_id {
            wake_task(task_id);
        }
    }

    fn queue_id_for_key(&self, key: RequestKey) -> Option<usize> {
        self.pending
            .lock()
            .request(key)
            .map(|request| request.submitted_request().queue_id)
    }

    fn wake_queue_progress(&self, queue_id: usize) {
        let waiters = {
            let mut waiters = self.queue_progress_waiters.lock();
            let mut matching = Vec::new();
            let mut idx = 0;
            while idx < waiters.len() {
                if waiters[idx].queue_id == queue_id {
                    matching.push(waiters.swap_remove(idx).task_id);
                } else {
                    idx += 1;
                }
            }
            matching
        };
        notify_waiters();
        for task_id in waiters {
            wake_task(task_id);
        }
    }

    pub(crate) fn read_blocks(&self, block_id: u64, buf: &mut [u8]) -> AxResult {
        self.check_not_poisoned()?;
        self.submit_io(RequestOp::Read, block_id, buf, None)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    pub(crate) fn write_blocks(&self, block_id: u64, buf: &[u8]) -> AxResult {
        self.check_not_poisoned()?;
        let mut no_dst = [];
        self.submit_io(RequestOp::Write, block_id, &mut no_dst, Some(buf))
    }

    #[cfg(feature = "ext4")]
    pub(crate) fn flush_blocks(&self) -> AxResult {
        self.check_not_poisoned()?;
        let _barrier = self.acquire_flush_barrier()?;
        self.wait_for_all_pending()?;
        let Some(queue_index) = self.flush_queue_index() else {
            return Ok(());
        };
        loop {
            let mut queue = self.queues[queue_index].lock();
            let info = queue.info;
            match submit_flush_request(&mut queue, info) {
                Ok(request_id) => {
                    let key = self
                        .pending
                        .lock()
                        .insert_submitted(info.id, request_id, None)
                        .map_err(map_blk_err_to_ax_err)?;
                    drop(queue);
                    self.wake_drain_after_irq_submit();
                    return self.wait_for_completion(key, None);
                }
                Err(BlkError::Retry) => {
                    drop(queue);
                    if !self.wait_for_queue_progress(info.id)? {
                        return Err(AxError::Io);
                    }
                }
                Err(err) => return Err(map_blk_err_to_ax_err(err)),
            }
        }
    }

    #[cfg(feature = "ext4")]
    fn flush_queue_index(&self) -> Option<usize> {
        self.queues
            .iter()
            .enumerate()
            .find_map(|(idx, queue)| queue.lock().info.limits.supports_flush.then_some(idx))
    }

    #[cfg(feature = "ext4")]
    fn wait_for_all_pending(&self) -> AxResult {
        loop {
            let queues =
                queue_ids_from_bits(self.pending.lock().pending_queue_bits()).collect::<Vec<_>>();
            if queues.is_empty() {
                return Ok(());
            }
            let mut progressed = false;
            for queue_id in queues {
                progressed |= self.wait_for_queue_progress(queue_id)?;
            }
            if !progressed {
                return Err(AxError::Io);
            }
        }
    }

    fn submit_io(
        &self,
        op: RequestOp,
        block_id: u64,
        read_dst: &mut [u8],
        write_src: Option<&[u8]>,
    ) -> AxResult {
        self.check_not_poisoned()?;
        let _data_io = self.begin_data_io()?;
        let info = self.queues[0].lock().info;
        let buf_len = write_src.map_or(read_dst.len(), <[u8]>::len);
        validate_io(info, block_id, buf_len)?;

        let sectors = buf_len as u64 / SECTOR_BYTES;
        let direction = match op {
            RequestOp::Read => {
                BLOCK_READS.fetch_add(1, Ordering::Relaxed);
                BLOCK_SECTORS_READ.fetch_add(sectors, Ordering::Relaxed);
                DmaDirection::FromDevice
            }
            RequestOp::Write => {
                BLOCK_WRITES.fetch_add(1, Ordering::Relaxed);
                BLOCK_SECTORS_WRITTEN.fetch_add(sectors, Ordering::Relaxed);
                DmaDirection::ToDevice
            }
            _ => return Err(AxError::InvalidInput),
        };
        let active_queues = self.active_queues();
        let total_window = active_queues
            .iter()
            .map(|queue| queue.window)
            .sum::<usize>();
        let mut deferred_chunk = None;
        let mut active = Vec::new();
        let mut first_error = None;
        let mut queue_cursor = 0;
        let mut next_byte_offset = 0usize;

        loop {
            while first_error.is_none() && active.len() < total_window {
                let (active_queue, chunk) =
                    if let Some((queue_index, chunk)) = deferred_chunk.take() {
                        let Some(active_queue) = active_queues.iter().copied().find(|queue| {
                            queue.index == queue_index && queue_has_window(*queue, &active)
                        }) else {
                            deferred_chunk = Some((queue_index, chunk));
                            break;
                        };
                        (active_queue, chunk)
                    } else {
                        let Some(active_queue) =
                            select_active_queue(&active_queues, &active, &mut queue_cursor)
                        else {
                            break;
                        };
                        let Some(chunk) = self.plan_next_chunk(
                            active_queue.index,
                            block_id,
                            buf_len,
                            next_byte_offset,
                        ) else {
                            break;
                        };
                        (active_queue, chunk)
                    };
                next_byte_offset = next_byte_offset.max(chunk.byte_offset + chunk.byte_len);
                let mut queue = self.queues[active_queue.index].lock();
                let info = queue.info;
                match self.submit_chunk(&mut queue, info, op, direction, chunk, write_src) {
                    Ok(entry) => {
                        drop(queue);
                        self.wake_drain_after_irq_submit();
                        active.push(entry);
                    }
                    Err(BlkError::Retry) => {
                        deferred_chunk = Some((active_queue.index, chunk));
                        next_byte_offset = next_byte_offset.min(chunk.byte_offset);
                        drop(queue);
                        if active.is_empty() && !self.wait_for_queue_progress(info.id)? {
                            first_error = Some(BlkError::Io);
                        }
                        break;
                    }
                    Err(err) => {
                        first_error = Some(err);
                        break;
                    }
                }
            }

            self.poll_active(&active);
            let progressed = self.harvest_active(&mut active, op, read_dst, &mut first_error)?;
            if first_error.is_some() && self.poisoned.load(Ordering::Acquire) {
                break;
            }
            if active.is_empty() {
                if first_error.is_some()
                    || (deferred_chunk.is_none() && next_byte_offset >= buf_len)
                {
                    break;
                }
            } else if !progressed {
                self.wait_for_any_active(&active)?;
                let _ = self.harvest_active(&mut active, op, read_dst, &mut first_error)?;
            }
        }
        first_error.map_or(Ok(()), |err| Err(map_blk_err_to_ax_err(err)))
    }

    fn begin_data_io(&self) -> AxResult<DataIoGuard<'_>> {
        loop {
            while self.flush_active.load(Ordering::Acquire) {
                self.wait_for_flush_release()?;
            }
            self.active_data_ops.fetch_add(1, Ordering::AcqRel);
            if !self.flush_active.load(Ordering::Acquire) {
                return Ok(DataIoGuard { device: self });
            }
            self.finish_data_io();
        }
    }

    fn finish_data_io(&self) {
        if self.active_data_ops.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.wake_barrier_waiters();
        }
    }

    #[cfg(feature = "ext4")]
    fn acquire_flush_barrier(&self) -> AxResult<FlushBarrierGuard<'_>> {
        loop {
            if self
                .flush_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
            self.wait_for_flush_release()?;
        }
        while self.active_data_ops.load(Ordering::Acquire) != 0 {
            self.wait_for_active_data_drain()?;
        }
        Ok(FlushBarrierGuard { device: self })
    }

    fn wait_for_flush_release(&self) -> AxResult {
        let task_id = current_task_id().unwrap_or(0);
        if !self.flush_active.load(Ordering::Acquire) {
            return Ok(());
        }
        self.barrier_waiters.lock().push(BarrierWaiter { task_id });
        if !self.flush_active.load(Ordering::Acquire) {
            self.remove_barrier_waiter(task_id);
            return Ok(());
        }
        if task_id != 0 {
            task_yield();
        } else {
            core::hint::spin_loop();
        }
        Ok(())
    }

    #[cfg(feature = "ext4")]
    fn wait_for_active_data_drain(&self) -> AxResult {
        let task_id = current_task_id().unwrap_or(0);
        if self.active_data_ops.load(Ordering::Acquire) == 0 {
            return Ok(());
        }
        self.barrier_waiters.lock().push(BarrierWaiter { task_id });
        if self.active_data_ops.load(Ordering::Acquire) == 0 {
            self.remove_barrier_waiter(task_id);
            return Ok(());
        }
        if task_id != 0 {
            task_yield();
        } else {
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn remove_barrier_waiter(&self, task_id: u64) {
        let mut waiters = self.barrier_waiters.lock();
        let mut idx = 0;
        while idx < waiters.len() {
            if waiters[idx].task_id == task_id {
                waiters.swap_remove(idx);
            } else {
                idx += 1;
            }
        }
    }

    fn wake_barrier_waiters(&self) {
        let waiters = {
            let mut waiters = self.barrier_waiters.lock();
            core::mem::take(&mut *waiters)
        };
        for waiter in waiters {
            wake_task(waiter.task_id);
        }
    }

    fn plan_next_chunk(
        &self,
        queue_index: usize,
        base_lba: u64,
        total_len: usize,
        byte_offset: usize,
    ) -> Option<TransferChunk> {
        if byte_offset >= total_len {
            return None;
        }
        let queue = self.queues[queue_index].lock();
        let block_size = queue.info.device.logical_block_size;
        let lba = base_lba.checked_add((byte_offset / block_size) as u64)?;
        let remaining = total_len - byte_offset;
        queue
            .planner
            .plan_from(lba, remaining, byte_offset)
            .ok()?
            .next()
    }

    fn submit_chunk(
        &self,
        queue: &mut QueueRuntime,
        info: QueueInfo,
        op: RequestOp,
        direction: DmaDirection,
        chunk: TransferChunk,
        write_src: Option<&[u8]>,
    ) -> Result<WindowEntry, BlkError> {
        let chunk_range = chunk.byte_offset..chunk.byte_offset + chunk.byte_len;
        let src = write_src.map(|src| &src[chunk_range.clone()]);
        let dma = runtime_device_dma(info.limits.dma_domain, info.limits.dma_mask)?;
        let (request_id, buffer) = match &mut queue.queue {
            RuntimeQueue::Legacy(legacy) => {
                let mut guard = DmaBufferGuard::new(
                    &dma,
                    chunk.byte_len,
                    info.limits.dma_alignment.max(1),
                    direction,
                    chunk,
                    src,
                )?;
                let segments = unsafe { guard.segments_for_submit() };
                let request = Request {
                    op,
                    lba: chunk.lba,
                    block_count: chunk.block_count,
                    segments,
                    flags: RequestFlags::NONE,
                };
                validate_request(info, &request)?;
                let request_id = legacy.submit_request(request)?;
                (request_id, Some(RuntimeDmaBuffer::Legacy(guard)))
            }
            RuntimeQueue::Owned(owned) => {
                let buffer = new_owned_dma_buffer(
                    &dma,
                    chunk.byte_len,
                    info.limits.dma_alignment.max(1),
                    direction,
                    src,
                )?;
                let request_id = owned
                    .submit_request(OwnedRequest {
                        op,
                        lba: chunk.lba,
                        block_count: chunk.block_count,
                        data: Some(buffer.prepare_for_device()),
                        flags: RequestFlags::NONE,
                    })
                    .map_err(|err| err.error)?;
                (request_id, None)
            }
        };
        let key = {
            let mut pending = self.pending.lock();
            if pending.contains_inflight_driver_request(info.id, request_id) {
                drop(pending);
                // The driver has accepted the request but reused an in-flight
                // request id. Keep the DMA guard alive instead of returning it
                // to the allocator while a broken driver may still complete
                // into the submitted buffer.
                if let Some(buffer) = buffer {
                    core::mem::forget(buffer);
                }
                self.poison_driver_contract_violation();
                return Err(BlkError::InvalidRequest);
            }
            pending.insert_submitted(info.id, request_id, buffer)?
        };
        Ok(WindowEntry {
            key,
            queue_id: info.id,
            byte_offset: chunk.byte_offset,
            byte_len: chunk.byte_len,
        })
    }

    fn active_queues(&self) -> Vec<ActiveQueue> {
        self.queues
            .iter()
            .enumerate()
            .map(|(index, queue)| {
                let info = queue.lock().info;
                ActiveQueue {
                    index,
                    queue_id: info.id,
                    window: self.submit_window.min(info.limits.max_inflight.max(1)),
                }
            })
            .collect()
    }

    fn poll_active(&self, active: &[WindowEntry]) -> usize {
        let keys = active.iter().map(|entry| entry.key).collect::<Vec<_>>();
        self.poll_batch(&keys)
    }

    fn poll_batch(&self, keys: &[RequestKey]) -> usize {
        let mut poller = DeviceRequestPoller { device: self };
        CompletionDrain::new(&self.pending, &mut poller).poll_keys(keys)
    }

    fn any_key_has_result(&self, keys: &[RequestKey]) -> bool {
        keys.iter()
            .any(|&key| self.pending.lock().result(key).is_some())
    }

    fn irq_fast_poll_any_key(&self, keys: &[RequestKey]) -> bool {
        if self.completion_mode() != BlockCompletionMode::IrqDriven {
            return false;
        }
        for _ in 0..IRQ_COMPLETION_FAST_POLL_ATTEMPTS {
            core::hint::spin_loop();
            let _ = self.poll_batch(keys);
            if self.any_key_has_result(keys) {
                return true;
            }
        }
        false
    }

    fn harvest_active(
        &self,
        active: &mut Vec<WindowEntry>,
        op: RequestOp,
        read_dst: &mut [u8],
        first_error: &mut Option<BlkError>,
    ) -> AxResult<bool> {
        let mut progressed = false;
        let mut idx = 0;
        while idx < active.len() {
            let entry = active[idx];
            let completed = self.pending.lock().result(entry.key).is_some();
            if !completed {
                idx += 1;
                continue;
            }
            progressed = true;
            let (result, guard) = self
                .pending
                .lock()
                .take_completed(entry.key)
                .ok_or(AxError::Io)?;
            if result.is_ok()
                && let Some(guard) = guard
            {
                if op == RequestOp::Read {
                    let range = entry.byte_offset..entry.byte_offset + entry.byte_len;
                    guard.complete(Some(&mut read_dst[range]));
                } else {
                    guard.complete(None);
                }
            }
            if let Err(err) = result
                && first_error.is_none()
            {
                *first_error = Some(err);
            }
            active.swap_remove(idx);
        }
        Ok(progressed)
    }

    fn wait_for_any_active(&self, active: &[WindowEntry]) -> AxResult {
        let task_id = current_task_id().unwrap_or(0);
        let mut observed = false;
        {
            let mut pending = self.pending.lock();
            for entry in active {
                observed |= pending.register_waiter_task(entry.key, task_id).is_some();
            }
        }
        if observed {
            return Ok(());
        }
        let keys = active.iter().map(|entry| entry.key).collect::<Vec<_>>();
        if self.any_key_has_result(&keys) {
            return Ok(());
        }
        let _ = self.poll_batch(&keys);
        if self.any_key_has_result(&keys) {
            return Ok(());
        }
        if self.irq_fast_poll_any_key(&keys) {
            return Ok(());
        }
        if self.completion_mode() == BlockCompletionMode::Polling {
            return self.polling_wait_for_any_key(&keys);
        }
        if self.irq_wait_or_timeout(task_id) {
            let _ = self.poll_batch(&keys);
        }
        Ok(())
    }

    fn wait_for_queue_progress(&self, queue_id: usize) -> AxResult<bool> {
        let task_id = current_task_id().unwrap_or(0);
        if self.pending.lock().keys_for_queue(queue_id).is_empty() {
            return Ok(false);
        }
        self.queue_progress_waiters
            .lock()
            .push(QueueProgressWaiter { queue_id, task_id });
        let keys = self.pending.lock().keys_for_queue(queue_id);
        let _ = self.poll_batch(&keys);
        if self.pending.lock().keys_for_queue(queue_id).is_empty() {
            self.remove_queue_progress_waiter(queue_id, task_id);
            return Ok(true);
        }
        if self.completion_mode() == BlockCompletionMode::Polling {
            self.remove_queue_progress_waiter(queue_id, task_id);
            return Ok(self.polling_wait_for_queue_empty(queue_id));
        }
        loop {
            let timed_out = self.irq_wait_or_timeout(task_id);
            let keys = self.pending.lock().keys_for_queue(queue_id);
            if keys.is_empty() {
                self.remove_queue_progress_waiter(queue_id, task_id);
                return Ok(true);
            }
            if timed_out {
                let _ = self.poll_batch(&keys);
                if self.pending.lock().keys_for_queue(queue_id).is_empty() {
                    self.remove_queue_progress_waiter(queue_id, task_id);
                    return Ok(true);
                }
            }
        }
    }

    fn remove_queue_progress_waiter(&self, queue_id: usize, task_id: u64) {
        let mut waiters = self.queue_progress_waiters.lock();
        let mut idx = 0;
        while idx < waiters.len() {
            if waiters[idx].queue_id == queue_id && waiters[idx].task_id == task_id {
                waiters.swap_remove(idx);
            } else {
                idx += 1;
            }
        }
    }

    fn polling_wait_for_any_key(&self, keys: &[RequestKey]) -> AxResult {
        loop {
            let _ = self.poll_batch(keys);
            if keys
                .iter()
                .any(|&key| self.pending.lock().result(key).is_some())
            {
                return Ok(());
            }
            core::hint::spin_loop();
        }
    }

    fn irq_wait_or_timeout(&self, task_id: u64) -> bool {
        // Early root probing may issue block I/O while IRQs are still disabled.
        if task_id != 0 && task_can_block() {
            task_wait_timeout(IRQ_COMPLETION_REPOLL_TIMEOUT)
        } else {
            core::hint::spin_loop();
            true
        }
    }

    fn polling_wait_for_queue_empty(&self, queue_id: usize) -> bool {
        loop {
            let keys = self.pending.lock().keys_for_queue(queue_id);
            if keys.is_empty() {
                return true;
            }
            let _ = self.poll_batch(&keys);
            if self.pending.lock().keys_for_queue(queue_id).is_empty() {
                return true;
            }
            core::hint::spin_loop();
        }
    }

    #[cfg(feature = "ext4")]
    fn wait_for_completion(&self, key: RequestKey, dst: Option<&mut [u8]>) -> AxResult {
        let task_id = current_task_id().unwrap_or(0);
        let observed = self.pending.lock().register_waiter_task(key, task_id);
        let observed = match observed {
            Some(result) => result,
            None => {
                let keys = [key];
                let _ = self.poll_one(key);
                let _ = self.irq_fast_poll_any_key(&keys);
                loop {
                    if let Some(result) = self
                        .pending
                        .lock()
                        .request(key)
                        .and_then(|request| request.result())
                    {
                        break result;
                    }
                    if self.completion_mode() == BlockCompletionMode::Polling {
                        self.polling_wait_for_any_key(&[key])?;
                        continue;
                    }
                    if self.irq_wait_or_timeout(task_id) {
                        let _ = self.poll_one(key);
                    }
                }
            }
        };
        let (result, guard) = self
            .pending
            .lock()
            .take_completed(key)
            .unwrap_or((observed, None));
        if result.is_ok()
            && let Some(guard) = guard
        {
            guard.complete(dst);
        }
        result.map_err(map_blk_err_to_ax_err)
    }

    fn check_not_poisoned(&self) -> AxResult {
        if self.poisoned.load(Ordering::Acquire) {
            Err(AxError::InvalidInput)
        } else {
            Ok(())
        }
    }

    fn poison_driver_contract_violation(&self) {
        if self.poisoned.swap(true, Ordering::AcqRel) {
            return;
        }
        let keys = self.pending.lock().active_keys();
        for key in keys {
            let token = self
                .pending
                .lock()
                .complete(key, Err(BlkError::InvalidRequest));
            self.wake_completed_request(key, token);
        }
        self.wake_barrier_waiters();
    }
}

type BlockIrqRegistrations = Vec<Box<dyn BlockIrqRegistration>>;
type RegisteredRdifBlockDevice = (Arc<BlockDeviceHandle>, BlockIrqRegistrations);
type RegisterIrqResult = Result<BlockIrqRegistrations, (AxError, BlockIrqRegistrations)>;

fn build_rdif_block_device(
    mut block: RdifBlockDevice,
    device_index: usize,
    drain_wake: Arc<dyn BlockDrainWake>,
) -> Result<RegisteredRdifBlockDevice, AxError> {
    let name = String::from(block.name());
    let config = rdif_block_runtime_config(&block, drain_wake);
    let mut queues = Vec::new();
    while let Some(queue) = block.interface_mut().create_owned_queue() {
        queues.push(RuntimeQueue::Owned(queue));
    }
    if queues.is_empty() {
        while let Some(queue) = block.interface_mut().create_queue() {
            queues.push(RuntimeQueue::Legacy(queue));
        }
    }
    if queues.is_empty() {
        return Err(AxError::BadState);
    }

    let bridge = Arc::new(BlockIrqBridge::new());
    let device = BlockDeviceHandle::new_runtime(name.clone(), queues, bridge, config)
        .map_err(map_blk_err_to_ax_err)?;

    let registrations = match register_rdif_irq_handlers(&mut block, device.clone(), device_index)
        .and_then(|registrations| {
            block.enable_irq();
            if block.is_irq_enabled() {
                Ok(registrations)
            } else {
                warn!(
                    "rdif filesystem block device {name} registered IRQ handler but device did \
                     not enable completion IRQ"
                );
                Err((AxError::Unsupported, registrations))
            }
        }) {
        Ok(registrations) => registrations,
        Err((err, registrations)) => {
            block.disable_irq();
            drop(registrations);
            warn!("rdif filesystem block device {name} falls back to polling without IRQ: {err:?}");
            Vec::new()
        }
    };
    if !registrations.is_empty() {
        device.set_completion_mode(BlockCompletionMode::IrqDriven);
        warn!("rdif filesystem block device {name} registered with IRQ-driven completion");
    }
    info!("registered rdif filesystem block device {name}");
    Ok((device, registrations))
}

fn rdif_block_runtime_config(
    block: &RdifBlockDevice,
    drain_wake: Arc<dyn BlockDrainWake>,
) -> BlockRuntimeConfig {
    let mut config = BlockRuntimeConfig::new(drain_wake);
    if !block.irq_sources().is_empty() && !block.interface().irq_sources().is_empty() {
        config.max_transfer_bytes = config.max_transfer_bytes.max(IRQ_DRIVEN_MAX_TRANSFER_BYTES);
    }
    config
}

fn register_rdif_irq_handlers(
    block: &mut RdifBlockDevice,
    device: Arc<BlockDeviceHandle>,
    device_index: usize,
) -> RegisterIrqResult {
    let irq_sources = block.interface().irq_sources();
    if irq_sources.is_empty() {
        warn!(
            "rdif filesystem block device {} exposes no IRQ source",
            block.name()
        );
        return Err((AxError::Unsupported, Vec::new()));
    }

    let mut registrations = Vec::new();
    for source in irq_sources {
        let Some((irq, handler)) = block.take_irq_handler(source.id) else {
            warn!(
                "rdif filesystem block device {} has IRQ source {} but no handler",
                block.name(),
                source.id
            );
            return Err((AxError::Unsupported, registrations));
        };
        let action = BlockIrqAction::new(handler, device.clone(), device_index);
        match register_shared_block_irq(format!("{}/{}", device.name(), source.id), irq, action) {
            Ok(registration) => registrations.push(registration),
            Err(err) => {
                warn!(
                    "rdif filesystem block device {} failed to register IRQ source {} on irq \
                     {:?}: {err:?}",
                    block.name(),
                    source.id,
                    irq
                );
                return Err((err, registrations));
            }
        }
    }
    Ok(registrations)
}

fn set_block_drain_pending(device_index: usize) {
    if device_index < u64::BITS as usize {
        BLOCK_DRAIN_DEVICE_BITS.fetch_or(1 << device_index, Ordering::AcqRel);
    } else {
        BLOCK_DRAIN_FULL_SCAN.store(true, Ordering::Release);
    }
}

fn mark_block_drain_device(device_index: usize) {
    set_block_drain_pending(device_index);
    notify_drain();
}

fn mark_block_drain_device_from_irq(device_index: usize) {
    set_block_drain_pending(device_index);
    notify_drain_from_irq();
}

fn block_drain_has_pending() -> bool {
    BLOCK_DRAIN_FULL_SCAN.load(Ordering::Acquire)
        || BLOCK_DRAIN_DEVICE_BITS.load(Ordering::Acquire) != 0
}

fn take_block_drain_selection() -> DrainSelection {
    DrainSelection {
        full_scan: BLOCK_DRAIN_FULL_SCAN.swap(false, Ordering::AcqRel),
        device_bits: BLOCK_DRAIN_DEVICE_BITS.swap(0, Ordering::AcqRel),
    }
}

fn drain_selection_contains(selection: DrainSelection, device_index: usize) -> bool {
    selection.full_scan
        || (device_index < u64::BITS as usize && selection.device_bits & (1 << device_index) != 0)
}

fn spawn_block_drain_task(runtime: Arc<BlockRuntime>) {
    BLOCK_DRAIN_SPAWNED.call_once(|| {
        spawn_task(
            String::from("block_drain"),
            Box::new(move || {
                loop {
                    if !block_drain_has_pending() {
                        let notified = wait_for_drain_notification_timeout(
                            core::time::Duration::from_millis(10),
                        );
                        if !notified {
                            BLOCK_DRAIN_FULL_SCAN.store(true, Ordering::Release);
                        }
                    }
                    if !block_drain_has_pending() {
                        continue;
                    }
                    let selection = take_block_drain_selection();
                    for (device_index, device) in runtime.devices().iter().enumerate() {
                        if drain_selection_contains(selection, device_index) {
                            device.drain_events();
                            device.drain_all_pending();
                        }
                    }
                }
            }),
        );
    });
}

impl BlockDeviceHandle {
    fn poll_request(
        &self,
        queue_id: usize,
        request_id: RequestId,
    ) -> Result<PollOutcome, BlkError> {
        let queue = self.queue_by_runtime_id(queue_id)?;
        let mut queue = queue.lock();
        match &mut queue.queue {
            RuntimeQueue::Legacy(legacy) => Ok(match legacy.poll_request(request_id)? {
                RequestStatus::Pending => PollOutcome::Pending,
                RequestStatus::Complete => PollOutcome::complete(Ok(())),
            }),
            RuntimeQueue::Owned(owned) => match owned.poll_request(request_id)? {
                OwnedRequestPoll::Pending => Ok(PollOutcome::Pending),
                OwnedRequestPoll::Ready(completed) => Ok(poll_outcome_from_owned(completed)),
            },
        }
    }

    fn poll_queue_completions(
        &self,
        queue_id: usize,
        request_ids: &[RequestId],
    ) -> Result<Vec<CompletionRecord>, BlkError> {
        let queue = self.queue_by_runtime_id(queue_id)?;
        let mut queue = queue.lock();
        match &mut queue.queue {
            RuntimeQueue::Legacy(legacy) => {
                let mut sink = CollectCompletionSink::default();
                legacy.poll_completions(request_ids, &mut sink)?;
                Ok(sink.completions)
            }
            RuntimeQueue::Owned(owned) => {
                let mut completions = Vec::new();
                for &request_id in request_ids {
                    match owned.poll_request(request_id)? {
                        OwnedRequestPoll::Pending => {}
                        OwnedRequestPoll::Ready(completed) => {
                            let rdif_block::CompletedRequest { id, result, data } = completed;
                            completions.push((id, result, data.map(RuntimeDmaBuffer::Owned)));
                        }
                    }
                }
                Ok(completions)
            }
        }
    }

    fn queue_by_runtime_id(&self, queue_id: usize) -> Result<&SpinNoIrq<QueueRuntime>, BlkError> {
        self.queues.get(queue_id).ok_or(BlkError::InvalidRequest)
    }
}

struct DeviceRequestPoller<'a> {
    device: &'a BlockDeviceHandle,
}

impl RequestPoller for DeviceRequestPoller<'_> {
    fn poll_request(
        &mut self,
        queue_id: usize,
        request_id: RequestId,
    ) -> Result<PollOutcome, BlkError> {
        self.device.poll_request(queue_id, request_id)
    }

    fn poll_completions(
        &mut self,
        queue_id: usize,
        request_ids: &[RequestId],
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        for (request_id, result, buffer) in
            self.device.poll_queue_completions(queue_id, request_ids)?
        {
            sink.complete_with_buffer(request_id, result, buffer);
        }
        Ok(())
    }

    fn poll_batch_query_failed(&mut self, queue_id: usize) {
        self.device
            .event_latch
            .bridge()
            .record_queue_ready(queue_id);
        self.device.drain_wake.wake_drain();
    }

    fn completed_request(&mut self, queue_id: usize, task_id: Option<u64>) {
        self.device.wake_queue_progress(queue_id);
        notify_waiters();
        if let Some(task_id) = task_id {
            wake_task(task_id);
        }
    }
}

#[cfg(feature = "ext4")]
fn submit_flush_request(queue: &mut QueueRuntime, info: QueueInfo) -> Result<RequestId, BlkError> {
    match &mut queue.queue {
        RuntimeQueue::Legacy(legacy) => {
            let mut segments = [];
            let request = Request {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                segments: &mut segments,
                flags: RequestFlags::NONE,
            };
            validate_request(info, &request)?;
            legacy.submit_request(request)
        }
        RuntimeQueue::Owned(owned) => owned
            .submit_request(OwnedRequest {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                data: None,
                flags: RequestFlags::NONE,
            })
            .map_err(|err| err.error),
    }
}

#[derive(Default)]
struct CollectCompletionSink {
    completions: Vec<CompletionRecord>,
}

impl RdifCompletionSink for CollectCompletionSink {
    fn complete(&mut self, request: RequestId, result: Result<(), BlkError>) {
        self.completions.push((request, result, None));
    }
}

fn poll_outcome_from_owned(completed: rdif_block::CompletedRequest) -> PollOutcome {
    let rdif_block::CompletedRequest { result, data, .. } = completed;
    PollOutcome::Complete {
        result,
        buffer: data.map(RuntimeDmaBuffer::Owned),
    }
}

fn validate_io(info: QueueInfo, block_id: u64, len: usize) -> AxResult {
    let block_size = info.device.logical_block_size;
    if block_size == 0 || !len.is_multiple_of(block_size) {
        return Err(AxError::InvalidInput);
    }
    let block_count = len / block_size;
    let end = block_id
        .checked_add(block_count as u64)
        .ok_or(AxError::InvalidInput)?;
    if end > info.device.num_blocks {
        return Err(AxError::InvalidInput);
    }
    Ok(())
}

fn same_device_identity(left: DeviceInfo, right: DeviceInfo) -> bool {
    left.num_blocks == right.num_blocks
        && left.logical_block_size == right.logical_block_size
        && left.read_only == right.read_only
}

fn select_active_queue(
    queues: &[ActiveQueue],
    active: &[WindowEntry],
    cursor: &mut usize,
) -> Option<ActiveQueue> {
    if queues.is_empty() {
        return None;
    }
    for _ in 0..queues.len() {
        let idx = *cursor % queues.len();
        *cursor = (*cursor).wrapping_add(1);
        let queue = queues[idx];
        if queue_has_window(queue, active) {
            return Some(queue);
        }
    }
    None
}

fn queue_has_window(queue: ActiveQueue, active: &[WindowEntry]) -> bool {
    let inflight = active
        .iter()
        .filter(|entry| entry.queue_id == queue.queue_id)
        .count();
    inflight < queue.window
}

pub fn map_blk_err_to_ax_err(err: BlkError) -> AxError {
    match err {
        BlkError::NotSupported => AxError::Unsupported,
        BlkError::Retry => AxError::WouldBlock,
        BlkError::NoMemory => AxError::NoMemory,
        BlkError::InvalidBlockIndex(_) | BlkError::InvalidRequest => AxError::InvalidInput,
        BlkError::Io | BlkError::Other(_) => AxError::Io,
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};

    use irq_framework::{HwIrq, IrqDomainId};
    use rdif_block::{Event, IdList, IrqHandler, IrqSourceInfo, QueueLimits};

    use super::*;

    struct PlannerTestQueue {
        info: QueueInfo,
    }

    impl PlannerTestQueue {
        fn new() -> Self {
            let block_size = 512;
            let limits = QueueLimits {
                max_inflight: 1,
                max_blocks_per_request: (IRQ_DRIVEN_MAX_TRANSFER_BYTES / block_size) as u32,
                max_segments: 1,
                max_segment_size: IRQ_DRIVEN_MAX_TRANSFER_BYTES,
                ..QueueLimits::simple(block_size, u64::MAX)
            };
            Self {
                info: QueueInfo {
                    id: 0,
                    device: DeviceInfo::new(
                        (IRQ_DRIVEN_MAX_TRANSFER_BYTES / block_size * 2) as u64,
                        block_size,
                    ),
                    limits,
                },
            }
        }
    }

    // SAFETY: This queue is used only to build a runtime planner; submitted
    // requests are not retained.
    unsafe impl IQueue for PlannerTestQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_request(&mut self, _request: Request<'_>) -> Result<RequestId, BlkError> {
            Ok(RequestId::new(1))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    impl rdif_block::DriverGeneric for PlannerTestQueue {
        fn name(&self) -> &str {
            "planner-test-queue"
        }
    }

    struct PlannerTestInterface {
        info: QueueInfo,
        expose_irq: bool,
    }

    impl PlannerTestInterface {
        fn new(info: QueueInfo, expose_irq: bool) -> Self {
            Self { info, expose_irq }
        }
    }

    impl rdif_block::DriverGeneric for PlannerTestInterface {
        fn name(&self) -> &str {
            "planner-test-interface"
        }
    }

    impl rdif_block::Interface for PlannerTestInterface {
        fn device_info(&self) -> DeviceInfo {
            self.info.device
        }

        fn queue_limits(&self) -> QueueLimits {
            self.info.limits
        }

        fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
            None
        }

        fn irq_sources(&self) -> rdif_block::IrqSourceList {
            if !self.expose_irq {
                return Vec::new();
            }
            let mut queues = IdList::none();
            queues.insert(0);
            vec![IrqSourceInfo::new(0, queues)]
        }

        fn take_irq_handler(&mut self, source_id: usize) -> Option<rdif_block::BIrqHandler> {
            (self.expose_irq && source_id == 0).then(|| Box::new(NoopIrq) as _)
        }
    }

    struct NoopIrq;

    impl IrqHandler for NoopIrq {
        fn handle_irq(&mut self) -> Event {
            Event::none()
        }
    }

    fn test_irq() -> IrqId {
        IrqId::new(IrqDomainId(42), HwIrq(7))
    }

    fn planner_chunk_size(config: BlockRuntimeConfig) -> usize {
        let device = BlockDeviceHandle::new_runtime(
            "planner-test",
            [RuntimeQueue::Legacy(Box::new(PlannerTestQueue::new()))],
            Arc::new(BlockIrqBridge::new()),
            config,
        )
        .unwrap();
        device.queues[0].lock().planner.chunk_size()
    }

    #[test]
    fn irq_capable_rdif_blocks_use_large_runtime_transfer_chunks() {
        let info = PlannerTestQueue::new().info();
        let irq_capable = RdifBlockDevice::new(
            "irq-capable",
            Some(test_irq()),
            Box::new(PlannerTestInterface::new(info, true)),
        );
        let config = rdif_block_runtime_config(&irq_capable, Arc::new(NoopDrainWake));

        assert_eq!(config.max_transfer_bytes, IRQ_DRIVEN_MAX_TRANSFER_BYTES);
        assert_eq!(planner_chunk_size(config), IRQ_DRIVEN_MAX_TRANSFER_BYTES);

        let polling_only = RdifBlockDevice::new(
            "polling-only",
            None,
            Box::new(PlannerTestInterface::new(info, true)),
        );
        let config = rdif_block_runtime_config(&polling_only, Arc::new(NoopDrainWake));

        assert_eq!(config.max_transfer_bytes, DEFAULT_MAX_TRANSFER_BYTES);
        assert_eq!(planner_chunk_size(config), DEFAULT_MAX_TRANSFER_BYTES);
    }
}

#[cfg(feature = "ext4")]
fn queue_ids_from_bits(bits: u64) -> impl Iterator<Item = usize> {
    (0..u64::BITS as usize).filter(move |queue_id| bits & (1 << queue_id) != 0)
}
