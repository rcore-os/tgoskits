use alloc::{boxed::Box, string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
#[cfg(not(test))]
use ax_kspin::SpinNoIrq;
use rdif_block::{
    BlkError, CompletionHint, CompletionSink as RdifCompletionSink, DeviceInfo, IQueue, QueueInfo,
    Request, RequestFlags, RequestId, RequestOp, RequestStatus, TransferChunk, TransferPlanner,
    TransferRuntimeCaps, validate_request,
};
#[cfg(test)]
use spin::Mutex as SpinNoIrq;

use super::{
    BlockDmaDirection, BlockDmaProvider, BlockIrqBridge, DmaBufferGuard, DrainEvents, PendingTable,
    PollClaim, PollProgress, RequestKey,
};
use crate::os::{current_task_id, notify_waiters, task_wait_until, task_yield, wake_task};

const DEFAULT_MAX_TRANSFER_BYTES: usize = 1024 * 1024;
const DEFAULT_SUBMIT_WINDOW: usize = 32;

type CompletionRecord = (RequestId, Result<(), BlkError>);

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

struct ClaimedQueueBatch {
    queue_id: usize,
    claimed: Vec<RequestKey>,
    driver_ids: Vec<RequestId>,
}

struct QueueProgressWaiter {
    queue_id: usize,
    task_id: u64,
}

struct BarrierWaiter {
    task_id: u64,
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
    pub dma: Arc<dyn BlockDmaProvider>,
    pub drain_wake: Arc<dyn BlockDrainWake>,
    pub completion_mode: BlockCompletionMode,
    pub max_transfer_bytes: usize,
    pub max_segments: usize,
    pub submit_window: usize,
}

impl BlockRuntimeConfig {
    pub fn new(dma: Arc<dyn BlockDmaProvider>, drain_wake: Arc<dyn BlockDrainWake>) -> Self {
        Self {
            dma,
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
    driver_queue_map: [Option<usize>; u64::BITS as usize],
    pending: SpinNoIrq<PendingTable>,
    queue_progress_waiters: SpinNoIrq<Vec<QueueProgressWaiter>>,
    barrier_waiters: SpinNoIrq<Vec<BarrierWaiter>>,
    bridge: Arc<BlockIrqBridge>,
    dma: Arc<dyn BlockDmaProvider>,
    drain_wake: Arc<dyn BlockDrainWake>,
    submit_window: usize,
    completion_mode: AtomicUsize,
    drain_running: AtomicBool,
    active_data_ops: AtomicUsize,
    flush_active: AtomicBool,
    poisoned: AtomicBool,
}

pub struct QueueRuntime {
    queue: Box<dyn IQueue>,
    driver_queue_id: usize,
    info: QueueInfo,
    planner: TransferPlanner,
}

impl QueueRuntime {
    pub fn new(
        queue: Box<dyn IQueue>,
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
}

impl BlockRuntime {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    pub fn push_device(&mut self, device: Arc<BlockDeviceHandle>) {
        self.devices.push(device);
    }

    pub fn devices(&self) -> &[Arc<BlockDeviceHandle>] {
        &self.devices
    }
}

impl Default for BlockRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockDeviceHandle {
    pub fn new(
        name: impl Into<String>,
        queues: impl IntoIterator<Item = Box<dyn IQueue>>,
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
            driver_queue_map,
            pending: SpinNoIrq::new(PendingTable::new()),
            queue_progress_waiters: SpinNoIrq::new(Vec::new()),
            barrier_waiters: SpinNoIrq::new(Vec::new()),
            bridge,
            dma: config.dma,
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
        self.bridge.clone()
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
            let events = self.bridge.take_events();
            self.drain_given_events(events)
        })
    }

    pub fn drain_hint(&self, hint: CompletionHint) -> usize {
        self.with_drain(|| self.drain_one_hint(hint))
    }

    pub fn record_driver_event(&self, event: rdif_block::Event) -> bool {
        self.record_translated_event(event, false)
    }

    pub fn record_driver_event_for_pending(&self, event: rdif_block::Event) -> bool {
        self.record_translated_event(event, true)
    }

    #[cfg(test)]
    pub(crate) fn pending_count_for_queue(&self, queue_id: usize) -> usize {
        self.pending.lock().keys_for_queue(queue_id).len()
    }

    #[cfg(test)]
    pub(crate) fn pending_queue_ready_events(&self) -> u64 {
        self.bridge.take_events().queue_bits
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
        let mut completed = 0;
        for hint in events.hints.iter() {
            completed += self.drain_one_hint(hint);
        }
        for queue_id in queue_ids_from_bits(events.queue_bits) {
            completed += self.drain_queue(queue_id);
        }
        completed
    }

    fn runtime_queue_id(&self, driver_queue_id: usize) -> Option<usize> {
        self.driver_queue_map
            .get(driver_queue_id)
            .copied()
            .flatten()
    }

    fn record_translated_event(&self, event: rdif_block::Event, pending_only: bool) -> bool {
        let pending_queue_bits = pending_only.then(|| self.pending.lock().pending_queue_bits());
        let mut translated = rdif_block::Event::none();
        for driver_queue_id in event.queues.iter() {
            if let Some(runtime_queue_id) = self.runtime_queue_id(driver_queue_id)
                && pending_queue_bits.is_none_or(|bits| bits & (1 << runtime_queue_id) != 0)
            {
                translated.queues.insert(runtime_queue_id);
            }
        }
        for hint in event.completions.iter() {
            if let Some(hint) = self.translate_driver_hint(hint)
                && pending_queue_bits.is_none_or(|bits| bits & (1 << hint.queue_id()) != 0)
            {
                translated.push_hint(hint);
            }
        }
        if translated.is_empty() {
            return false;
        }
        self.bridge.record_event(translated);
        true
    }

    fn translate_driver_hint(&self, hint: CompletionHint) -> Option<CompletionHint> {
        let queue_id = self.runtime_queue_id(hint.queue_id())?;
        Some(match hint {
            CompletionHint::Queue { .. } => CompletionHint::Queue { queue_id },
            CompletionHint::Request { request_id, .. } => CompletionHint::Request {
                queue_id,
                request_id,
            },
            CompletionHint::Batch { ids, .. } => CompletionHint::Batch { queue_id, ids },
        })
    }

    fn drain_one_hint(&self, hint: CompletionHint) -> usize {
        match hint {
            CompletionHint::Queue { queue_id } => self.drain_queue(queue_id),
            CompletionHint::Request {
                queue_id,
                request_id,
            } => {
                let keys = self.matching_driver_keys(queue_id, &[request_id]);
                self.poll_batch(&keys)
            }
            CompletionHint::Batch { queue_id, ids } => {
                let ids = ids.iter().collect::<Vec<_>>();
                let keys = self.matching_driver_keys(queue_id, &ids);
                self.poll_batch(&keys)
            }
        }
    }

    fn drain_queue(&self, queue_id: usize) -> usize {
        let keys = self.pending.lock().keys_for_queue(queue_id);
        self.poll_batch(&keys)
    }

    fn matching_driver_keys(&self, queue_id: usize, ids: &[RequestId]) -> Vec<RequestKey> {
        self.pending.lock().matching_driver_keys(queue_id, ids)
    }

    fn record_queue_ready_for_keys(&self, keys: &[RequestKey]) {
        let queue_bits = {
            let pending = self.pending.lock();
            keys.iter().fold(0u64, |bits, &key| {
                let Some(request) = pending.request(key) else {
                    return bits;
                };
                if request.result().is_some() {
                    return bits;
                }
                let queue_id = request.submitted_request().queue_id;
                if queue_id < u64::BITS as usize {
                    bits | (1 << queue_id)
                } else {
                    bits
                }
            })
        };

        for queue_id in queue_ids_from_bits(queue_bits) {
            self.bridge.record_queue_ready(queue_id);
        }
    }

    fn poll_batch(&self, keys: &[RequestKey]) -> usize {
        let batches = self.claim_poll_batches(keys);
        if batches.is_empty() {
            return 0;
        }

        let mut completed = 0;
        for batch in batches {
            let result = self.poll_queue_completions(batch.queue_id, &batch.driver_ids);
            let query_failed = result.is_err();
            let mut sink = DeviceCompletionSink {
                device: self,
                completed: 0,
                terminal: Vec::new(),
                claimed: batch
                    .claimed
                    .iter()
                    .copied()
                    .zip(batch.driver_ids.iter().copied())
                    .map(|(key, request_id)| (request_id, key))
                    .collect(),
            };
            if let Ok(completions) = result {
                for (request_id, result) in completions {
                    sink.complete(request_id, result);
                }
            }
            if query_failed {
                self.bridge.record_queue_ready(batch.queue_id);
                self.drain_wake.wake_drain();
            }
            let terminal = sink.terminal.clone();
            completed += sink.completed;
            for key in batch.claimed {
                if terminal.contains(&key) {
                    continue;
                }
                if query_failed {
                    self.release_claimed_poll(key);
                } else {
                    let progress = self.pending.lock().finish_pending_poll(key);
                    match progress {
                        PollProgress::Pending | PollProgress::Complete => {}
                        PollProgress::Repoll => {
                            let submitted = self
                                .pending
                                .lock()
                                .request(key)
                                .map(|request| request.submitted_request());
                            let completed_key = match submitted {
                                Some(request) => {
                                    let result =
                                        self.poll_request(request.queue_id, request.request_id);
                                    self.finish_poll(key, result).unwrap_or(false)
                                }
                                None => false,
                            };
                            completed += usize::from(completed_key);
                        }
                    }
                }
            }
        }
        completed
    }

    fn release_claimed_poll(&self, key: RequestKey) {
        while let PollProgress::Repoll = self.pending.lock().finish_pending_poll(key) {}
    }

    fn claim_poll_batches(&self, keys: &[RequestKey]) -> Vec<ClaimedQueueBatch> {
        let mut batches: Vec<ClaimedQueueBatch> = Vec::new();
        let mut pending = self.pending.lock();
        for &key in keys {
            if pending.begin_poll(key) != PollClaim::Claimed {
                continue;
            }
            let submitted = pending
                .request(key)
                .expect("claimed request must remain present")
                .submitted_request();
            if let Some(batch) = batches
                .iter_mut()
                .find(|batch| batch.queue_id == submitted.queue_id)
            {
                batch.claimed.push(key);
                batch.driver_ids.push(submitted.request_id);
            } else {
                let mut batch = ClaimedQueueBatch {
                    queue_id: submitted.queue_id,
                    claimed: Vec::new(),
                    driver_ids: Vec::new(),
                };
                batch.claimed.push(key);
                batch.driver_ids.push(submitted.request_id);
                batches.push(batch);
            }
        }
        batches
    }

    #[cfg(feature = "ext4")]
    fn poll_one(&self, key: RequestKey) -> bool {
        if self.pending.lock().begin_poll(key) != PollClaim::Claimed {
            return false;
        }
        self.poll_claimed_one(key)
    }

    #[cfg(feature = "ext4")]
    fn poll_claimed_one(&self, key: RequestKey) -> bool {
        loop {
            let submitted = match self.pending.lock().request(key) {
                Some(request) => request.submitted_request(),
                None => return false,
            };
            let result = self.poll_request(submitted.queue_id, submitted.request_id);
            if let Some(completed) = self.finish_poll(key, result) {
                return completed;
            }
        }
    }

    fn finish_poll(
        &self,
        key: RequestKey,
        result: Result<RequestStatus, BlkError>,
    ) -> Option<bool> {
        let task_id = match result {
            Ok(RequestStatus::Pending) => match self.pending.lock().finish_pending_poll(key) {
                PollProgress::Pending => None,
                PollProgress::Repoll => return None,
                PollProgress::Complete => None,
            },
            Ok(RequestStatus::Complete) => self.pending.lock().complete(key, Ok(())),
            Err(err) => self.pending.lock().complete(key, Err(err)),
        };
        if !matches!(result, Ok(RequestStatus::Pending)) {
            self.wake_completed_request(key, task_id);
        }
        Some(!matches!(result, Ok(RequestStatus::Pending)))
    }

    fn wake_completed_request(&self, key: RequestKey, task_id: Option<u64>) {
        let queue_id = self.queue_id_for_key(key);
        if let Some(queue_id) = queue_id {
            self.wake_queue_progress(queue_id);
        }
        if let Some(task_id) = task_id {
            wake_task(task_id);
        }
        notify_waiters();
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
        for task_id in waiters {
            wake_task(task_id);
        }
        notify_waiters();
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
            let mut segments = [];
            let request = Request {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                segments: &mut segments,
                flags: RequestFlags::NONE,
            };
            validate_request(info, &request).map_err(map_blk_err_to_ax_err)?;
            match queue.queue.submit_request(request) {
                Ok(request_id) => {
                    let key = self
                        .pending
                        .lock()
                        .insert_submitted(info.id, request_id, None)
                        .map_err(map_blk_err_to_ax_err)?;
                    drop(queue);
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

        let direction = match op {
            RequestOp::Read => BlockDmaDirection::Read,
            RequestOp::Write => BlockDmaDirection::Write,
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
                    Ok(entry) => active.push(entry),
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
        direction: BlockDmaDirection,
        chunk: TransferChunk,
        write_src: Option<&[u8]>,
    ) -> Result<WindowEntry, BlkError> {
        let chunk_range = chunk.byte_offset..chunk.byte_offset + chunk.byte_len;
        let src = write_src.map(|src| &src[chunk_range.clone()]);
        let mut guard = DmaBufferGuard::new(
            self.dma.alloc(
                info.limits.dma_mask,
                chunk.byte_len,
                info.limits.dma_alignment.max(1),
                direction,
            )?,
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
        let request_id = queue.queue.submit_request(request)?;
        let key = {
            let mut pending = self.pending.lock();
            if pending.contains_inflight_driver_request(info.id, request_id) {
                drop(pending);
                // The driver has accepted the request but reused an in-flight
                // request id. Keep the DMA guard alive instead of returning it
                // to the allocator while a broken driver may still complete
                // into the submitted buffer.
                core::mem::forget(guard);
                self.poison_driver_contract_violation();
                return Err(BlkError::InvalidRequest);
            }
            pending.insert_submitted(info.id, request_id, Some(guard))?
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
        if keys
            .iter()
            .any(|&key| self.pending.lock().result(key).is_some())
        {
            return Ok(());
        }
        let _ = self.poll_batch(&keys);
        if keys
            .iter()
            .any(|&key| self.pending.lock().result(key).is_some())
        {
            return Ok(());
        }
        if self.completion_mode() == BlockCompletionMode::Polling {
            self.record_queue_ready_for_keys(&keys);
            self.drain_wake.wake_drain();
        }
        if task_id != 0 {
            task_wait_until(|| {
                keys.iter()
                    .any(|&key| self.pending.lock().result(key).is_some())
            });
        } else {
            core::hint::spin_loop();
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
            self.bridge.record_queue_ready(queue_id);
            self.drain_wake.wake_drain();
        }
        if task_id != 0 {
            task_wait_until(|| self.pending.lock().keys_for_queue(queue_id).is_empty());
        } else {
            core::hint::spin_loop();
        }
        Ok(true)
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

    #[cfg(feature = "ext4")]
    fn wait_for_completion(&self, key: RequestKey, dst: Option<&mut [u8]>) -> AxResult {
        let task_id = current_task_id().unwrap_or(0);
        let observed = self.pending.lock().register_waiter_task(key, task_id);
        let observed = match observed {
            Some(result) => result,
            None => {
                let _ = self.poll_one(key);
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
                        self.record_queue_ready_for_keys(&[key]);
                        self.drain_wake.wake_drain();
                    }
                    if task_id != 0 {
                        task_wait_until(|| {
                            self.pending
                                .lock()
                                .request(key)
                                .and_then(|request| request.result())
                                .is_some()
                        });
                    } else {
                        core::hint::spin_loop();
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

impl BlockDeviceHandle {
    fn poll_request(
        &self,
        queue_id: usize,
        request_id: RequestId,
    ) -> Result<RequestStatus, BlkError> {
        let queue = self.queue_by_runtime_id(queue_id)?;
        queue.lock().queue.poll_request(request_id)
    }

    fn poll_queue_completions(
        &self,
        queue_id: usize,
        request_ids: &[RequestId],
    ) -> Result<Vec<CompletionRecord>, BlkError> {
        let queue = self.queue_by_runtime_id(queue_id)?;
        let mut queue = queue.lock();
        let mut sink = CollectCompletionSink::default();
        queue.queue.poll_completions(request_ids, &mut sink)?;
        Ok(sink.completions)
    }

    fn queue_by_runtime_id(&self, queue_id: usize) -> Result<&SpinNoIrq<QueueRuntime>, BlkError> {
        self.queues.get(queue_id).ok_or(BlkError::InvalidRequest)
    }
}

#[derive(Default)]
struct CollectCompletionSink {
    completions: Vec<CompletionRecord>,
}

impl RdifCompletionSink for CollectCompletionSink {
    fn complete(&mut self, request: RequestId, result: Result<(), BlkError>) {
        self.completions.push((request, result));
    }
}

struct DeviceCompletionSink<'a> {
    device: &'a BlockDeviceHandle,
    completed: usize,
    terminal: Vec<RequestKey>,
    claimed: Vec<(RequestId, RequestKey)>,
}

impl DeviceCompletionSink<'_> {
    fn complete(&mut self, request_id: RequestId, result: Result<(), BlkError>) {
        if let Some((_, key)) = self
            .claimed
            .iter()
            .copied()
            .find(|(candidate, _)| *candidate == request_id)
        {
            self.complete_runtime(key, result);
        }
    }

    fn complete_runtime(&mut self, key: RequestKey, result: Result<(), BlkError>) {
        let task_id = self.device.pending.lock().complete(key, result);
        self.device.wake_completed_request(key, task_id);
        self.terminal.push(key);
        self.completed += 1;
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

fn queue_ids_from_bits(bits: u64) -> impl Iterator<Item = usize> {
    (0..u64::BITS as usize).filter(move |queue_id| bits & (1 << queue_id) != 0)
}
