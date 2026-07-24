use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::String,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::{
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use ax_errno::{AxError, AxResult};
use irq_framework::IrqId;
use rdif_block::{
    BatchSubmitError, BlkError, BlockController, ControllerEvent, ControllerState,
    ControllerUpdate, DeviceInfo, IrqEndpoint, OwnedRequest, OwnedRequestBatch, QueueInfo,
    RequestFlags, RequestOp, SubmitError, TransferPlanner, TransferRuntimeCaps,
    validate_owned_request,
};
use spin::Once;

#[cfg(any(feature = "ext4", feature = "fat"))]
use super::dma::prepare_write;
use super::{
    channel::{BoundedChannel, SendError},
    completion::{CompletionGroup, CompletionSubscription},
    dma::prepare_read,
    hctx::{ControllerEventPort, Hctx, HctxObserver, Submission, request_is_nowait},
    irq::{BlockIrqAction, ControllerIrqLatch, ControllerIrqTarget, IrqTarget},
};
use crate::os::{
    BlockIrqRegistration, BlockNotification, BlockThread, register_block_irq, runtime_ops,
    sync::IrqMutex, wall_time,
};

const CONTROLLER_CHANNEL_DEPTH: usize = 64;
const CONTROLLER_TRANSITION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RUNTIME_HCTX: usize = u64::BITS as usize;
const MAX_RUNTIME_TRANSFER_BYTES: usize = 4 * 1024 * 1024;

const DEVICE_STARTING: u8 = 0;
const DEVICE_READY: u8 = 1;
const DEVICE_FAILED: u8 = 2;
const DEVICE_STOPPED: u8 = 3;

static BLOCK_RUNTIME: Once<Arc<BlockRuntime>> = Once::new();
static BLOCK_READS: AtomicU64 = AtomicU64::new(0);
static BLOCK_SECTORS_READ: AtomicU64 = AtomicU64::new(0);
static BLOCK_WRITES: AtomicU64 = AtomicU64::new(0);
static BLOCK_SECTORS_WRITTEN: AtomicU64 = AtomicU64::new(0);

/// Cumulative completed block I/O counters. Sector counters use 512-byte
/// sectors to retain the Linux `/proc/diskstats` convention.
pub fn block_io_stats() -> (u64, u64, u64, u64) {
    (
        BLOCK_READS.load(Ordering::Relaxed),
        BLOCK_SECTORS_READ.load(Ordering::Relaxed),
        BLOCK_WRITES.load(Ordering::Relaxed),
        BLOCK_SECTORS_WRITTEN.load(Ordering::Relaxed),
    )
}

/// One platform IRQ resolved before the portable controller enters runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockIrqSource {
    pub source_id: usize,
    pub irq: IrqId,
}

/// Portable controller plus platform IRQ metadata transferred from `ax-driver`.
pub struct RdifBlockDevice {
    name: String,
    irqs: Vec<BlockIrqSource>,
    controller: Box<dyn BlockController>,
}

impl RdifBlockDevice {
    pub fn new_with_irqs(
        name: impl Into<String>,
        irqs: impl IntoIterator<Item = BlockIrqSource>,
        controller: Box<dyn BlockController>,
    ) -> Self {
        Self {
            name: name.into(),
            irqs: irqs.into_iter().collect(),
            controller,
        }
    }
}

/// Installed IRQ-driven block runtime.
pub struct BlockRuntime {
    devices: Vec<Arc<BlockDeviceHandle>>,
}

impl BlockRuntime {
    pub fn from_rdif_devices(devices: impl IntoIterator<Item = RdifBlockDevice>) -> Self {
        let mut registered = Vec::new();
        for device in devices {
            match BlockDeviceHandle::start(device) {
                Ok(handle) => registered.push(handle),
                Err(error) => {
                    warn!("failed to start IRQ-driven block controller: {error:?}");
                }
            }
        }
        Self {
            devices: registered,
        }
    }

    pub fn install_from_rdif_devices(
        devices: impl IntoIterator<Item = RdifBlockDevice>,
    ) -> Arc<Self> {
        let runtime = Arc::new(Self::from_rdif_devices(devices));
        BLOCK_RUNTIME.call_once(|| Arc::clone(&runtime));
        runtime
    }

    pub fn devices(&self) -> &[Arc<BlockDeviceHandle>] {
        &self.devices
    }

    fn online_smp(&self) -> Result<(), BlkError> {
        for device in &self.devices {
            device.online_smp()?;
        }
        Ok(())
    }

    fn release_irqs_for_passthrough(&self) -> usize {
        self.devices.iter().map(|device| device.shutdown()).sum()
    }
}

/// Expands every installed controller after schedulers, IPIs, and local IRQs
/// are online on all CPUs.
///
/// # Errors
///
/// Returns the first controller transition failure. No controller falls back
/// to polling.
pub fn online_smp() -> Result<(), BlkError> {
    BLOCK_RUNTIME
        .get()
        .ok_or(BlkError::Other("block runtime is not installed"))?
        .online_smp()
}

/// Stops host block IRQ ownership before device passthrough.
pub fn release_block_irqs_for_passthrough() -> usize {
    BLOCK_RUNTIME
        .get()
        .map_or(0, |runtime| runtime.release_irqs_for_passthrough())
}

/// Filesystem-facing device handle backed only by bounded channels.
pub struct BlockDeviceHandle {
    inner: Arc<DeviceInner>,
}

impl Drop for BlockDeviceHandle {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.inner.shutdown();
        }
    }
}

struct DeviceInner {
    name: String,
    info: IrqMutex<DeviceInfo>,
    max_io_queues: usize,
    irq_sources: Vec<BlockIrqSource>,
    hctxs: IrqMutex<Vec<Arc<Hctx>>>,
    cpu_channels: IrqMutex<Vec<CpuSubmissionChannel>>,
    irq_registrations: IrqMutex<Vec<Box<dyn BlockIrqRegistration>>>,
    controller: Arc<ControllerPort>,
    controller_thread: IrqMutex<Option<Box<dyn BlockThread>>>,
    state: AtomicU8,
    accepting: AtomicBool,
    active_data: AtomicUsize,
    flush_active: AtomicBool,
    barrier_notification: Arc<dyn BlockNotification>,
    state_notification: Arc<dyn BlockNotification>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubmissionAdmission {
    Blocking,
    Nowait,
}

impl SubmissionAdmission {
    const fn is_nowait(self) -> bool {
        matches!(self, Self::Nowait)
    }

    fn cannot_wait(self) -> bool {
        self.is_nowait() || request_cannot_block()
    }
}

impl BlockDeviceHandle {
    fn start(device: RdifBlockDevice) -> Result<Arc<Self>, BlkError> {
        let RdifBlockDevice {
            name,
            irqs,
            controller,
        } = device;
        let info = controller.device_info();
        let max_io_queues = controller.max_io_queues().min(MAX_RUNTIME_HCTX);
        if max_io_queues == 0 {
            return Err(BlkError::NotSupported);
        }
        let ops =
            runtime_ops().map_err(|_| BlkError::Other("block runtime adapter is not installed"))?;
        let controller_notification = ops.notification();
        let controller_port = Arc::new(ControllerPort {
            commands: BoundedChannel::with_item_notification(
                CONTROLLER_CHANNEL_DEPTH,
                Arc::clone(&controller_notification),
            )
            .map_err(|_| BlkError::NoMemory)?,
            notification: controller_notification,
            irq_latches: IrqMutex::new(Vec::new()),
        });
        let inner = Arc::new(DeviceInner {
            name,
            info: IrqMutex::new(info),
            max_io_queues,
            irq_sources: irqs,
            hctxs: IrqMutex::new(Vec::new()),
            cpu_channels: IrqMutex::new(Vec::new()),
            irq_registrations: IrqMutex::new(Vec::new()),
            controller: Arc::clone(&controller_port),
            controller_thread: IrqMutex::new(None),
            state: AtomicU8::new(DEVICE_STARTING),
            accepting: AtomicBool::new(false),
            active_data: AtomicUsize::new(0),
            flush_active: AtomicBool::new(false),
            barrier_notification: ops.notification(),
            state_notification: ops.notification(),
        });
        let weak = Arc::downgrade(&inner);
        let thread = ops
            .spawn_pinned(
                format!("blk-ctl/{}", inner.name),
                0,
                Box::new(move || run_controller(controller, controller_port, weak)),
            )
            .map_err(|_| BlkError::NoMemory)?;
        *inner.controller_thread.lock() = Some(thread);

        let handle = Arc::new(Self { inner });
        let state = match handle
            .inner
            .controller
            .call(ControllerEvent::Start { target_queues: 1 })
        {
            Ok(state) => state,
            Err(error) => {
                handle.inner.shutdown();
                return Err(error);
            }
        };
        if state != ControllerState::Ready
            && let Err(error) = handle.inner.wait_until_ready(CONTROLLER_TRANSITION_TIMEOUT)
        {
            handle.inner.shutdown();
            return Err(error);
        }
        if handle.inner.hctxs.lock().is_empty() {
            handle.shutdown();
            return Err(BlkError::Other(
                "controller reported ready without an I/O hardware queue",
            ));
        }
        handle.inner.accepting.store(true, Ordering::Release);
        handle.inner.state.store(DEVICE_READY, Ordering::Release);
        Ok(handle)
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn device_info(&self) -> DeviceInfo {
        *self.inner.info.lock()
    }

    /// Enqueues one DMA-owning request on the current CPU software channel.
    ///
    /// `NOWAIT` affects only bounded channel admission and is removed before
    /// hardware validation.
    pub fn submit_owned(
        &self,
        request: OwnedRequest,
    ) -> Result<CompletionSubscription, SubmitError> {
        let batch = OwnedRequestBatch::from_iter([request]);
        match self.submit_batch_owned(batch) {
            Ok(group) => Ok(group
                .into_single()
                .expect("single-request submission returns one completion")),
            Err(error) => {
                let result = error.error;
                let mut requests = error.into_batch().into_iter();
                let request = requests
                    .next()
                    .expect("single-request submission error returns its request");
                Err(SubmitError::new(result, request))
            }
        }
    }

    /// Enqueues one ordered request group on the current CPU software channel.
    ///
    /// The runtime may split or combine groups when dispatching to hardware.
    /// A flush must be submitted alone so the device-level barrier can order it
    /// against every hardware queue.
    pub fn submit_batch_owned(
        &self,
        requests: OwnedRequestBatch,
    ) -> Result<CompletionGroup, BatchSubmitError> {
        if !self.inner.accepting.load(Ordering::Acquire) {
            return Err(BatchSubmitError::new(BlkError::Io, requests));
        }
        let count = requests.len();
        if count == 0 {
            return Err(BatchSubmitError::new(BlkError::InvalidRequest, requests));
        }
        let Some(cpu_channel) = self.inner.select_cpu_channel() else {
            return Err(BatchSubmitError::new(BlkError::Io, requests));
        };
        let mut info = cpu_channel.hctx.info();
        info.device = *self.inner.info.lock();
        let validation_error = requests
            .iter()
            .find_map(|request| validate_owned_request(info, request).err());
        if let Some(error) = validation_error {
            return Err(BatchSubmitError::new(error, requests));
        }

        let admission = if requests.iter().any(request_is_nowait) {
            SubmissionAdmission::Nowait
        } else {
            SubmissionAdmission::Blocking
        };
        let flush_count = requests
            .iter()
            .filter(|request| request.op == RequestOp::Flush)
            .count();
        if flush_count != 0 && (flush_count != 1 || count != 1) {
            return Err(BatchSubmitError::new(BlkError::InvalidRequest, requests));
        }
        let is_flush = flush_count == 1;
        if is_flush {
            if let Err(error) = self.inner.begin_flush_barrier(admission) {
                return Err(BatchSubmitError::new(error, requests));
            }
        } else if let Err(error) = self.inner.enter_data_submissions(count, admission) {
            return Err(BatchSubmitError::new(error, requests));
        }

        let (group, mut completions) = match CompletionGroup::pairs(count) {
            Ok(pair) => pair,
            Err(error) => {
                self.inner.undo_submission_admission(
                    if is_flush {
                        RequestOp::Flush
                    } else {
                        RequestOp::Read
                    },
                    count,
                );
                return Err(BatchSubmitError::new(error, requests));
            }
        };
        let mut submissions = VecDeque::new();
        if submissions.try_reserve_exact(count).is_err() {
            self.inner.undo_submission_admission(
                if is_flush {
                    RequestOp::Flush
                } else {
                    RequestOp::Read
                },
                count,
            );
            return Err(BatchSubmitError::new(BlkError::NoMemory, requests));
        }
        for request in requests {
            let completion = completions
                .pop_front()
                .expect("completion sender count matches request batch");
            submissions.push_back(Submission {
                request,
                completion,
            });
        }
        if let Err(SendError::Closed(submissions) | SendError::Full(submissions)) = cpu_channel
            .channel
            .send_many(submissions, admission.is_nowait())
        {
            self.inner.undo_submission_admission(
                if is_flush {
                    RequestOp::Flush
                } else {
                    RequestOp::Read
                },
                count,
            );
            let requests = submissions
                .into_iter()
                .map(|submission| submission.request)
                .collect();
            return Err(BatchSubmitError::new(BlkError::Retry, requests));
        }
        Ok(group)
    }

    pub(crate) fn read_blocks(&self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let mut offset = 0;
        while offset < buf.len() {
            let info = self.inner.selected_queue_info().ok_or(AxError::Io)?;
            let chunk = next_chunk(info, block_id, buf.len(), offset)
                .map_err(|error| block_io_error("plan", RequestOp::Read, block_id, error))?;
            let data = prepare_read(info.limits, chunk.byte_len).map_err(|error| {
                block_io_error("prepare DMA", RequestOp::Read, chunk.lba, error)
            })?;
            let request = OwnedRequest {
                op: RequestOp::Read,
                lba: chunk.lba,
                block_count: chunk.block_count,
                data: Some(data),
                flags: RequestFlags::NONE,
            };
            let completion = self
                .submit_owned(request)
                .map_err(|error| block_io_error("submit", RequestOp::Read, chunk.lba, error.error))?
                .recv()
                .map_err(|error| block_io_error("receive", RequestOp::Read, chunk.lba, error))?;
            completion
                .result
                .map_err(|error| block_io_error("complete", RequestOp::Read, chunk.lba, error))?;
            let data = completion.data.ok_or_else(|| {
                block_io_error(
                    "return DMA ownership",
                    RequestOp::Read,
                    chunk.lba,
                    BlkError::Io,
                )
            })?;
            data.copy_from_device_to_slice(&mut buf[offset..offset + chunk.byte_len]);
            offset += chunk.byte_len;
        }
        Ok(())
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    pub(crate) fn write_blocks(&self, block_id: u64, buf: &[u8]) -> AxResult {
        let mut offset = 0;
        while offset < buf.len() {
            let info = self.inner.selected_queue_info().ok_or(AxError::Io)?;
            let chunk = next_chunk(info, block_id, buf.len(), offset)
                .map_err(|error| block_io_error("plan", RequestOp::Write, block_id, error))?;
            let data = prepare_write(info.limits, &buf[offset..offset + chunk.byte_len]).map_err(
                |error| block_io_error("prepare DMA", RequestOp::Write, chunk.lba, error),
            )?;
            let request = OwnedRequest {
                op: RequestOp::Write,
                lba: chunk.lba,
                block_count: chunk.block_count,
                data: Some(data),
                flags: RequestFlags::NONE,
            };
            let completion = self
                .submit_owned(request)
                .map_err(|error| {
                    block_io_error("submit", RequestOp::Write, chunk.lba, error.error)
                })?
                .recv()
                .map_err(|error| block_io_error("receive", RequestOp::Write, chunk.lba, error))?;
            completion
                .result
                .map_err(|error| block_io_error("complete", RequestOp::Write, chunk.lba, error))?;
            offset += chunk.byte_len;
        }
        Ok(())
    }

    #[cfg(feature = "ext4")]
    pub(crate) fn flush_blocks(&self) -> AxResult {
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        let completion = self
            .submit_owned(request)
            .map_err(|error| map_blk_err_to_ax_err(error.error))?
            .recv()
            .map_err(map_blk_err_to_ax_err)?;
        completion.result.map_err(map_blk_err_to_ax_err)
    }

    fn online_smp(&self) -> Result<(), BlkError> {
        self.inner.online_smp()
    }

    fn shutdown(&self) -> usize {
        self.inner.shutdown()
    }
}

impl DeviceInner {
    fn select_cpu_channel(&self) -> Option<CpuSubmissionChannel> {
        let channels = self.cpu_channels.lock();
        if channels.is_empty() {
            return None;
        }
        let cpu = runtime_ops().ok()?.current_cpu();
        Some(channels[cpu % channels.len()].clone())
    }

    fn selected_queue_info(&self) -> Option<QueueInfo> {
        self.select_cpu_channel().map(|channel| {
            let mut info = channel.hctx.info();
            info.device = *self.info.lock();
            info
        })
    }

    fn enter_data_submissions(
        &self,
        count: usize,
        admission: SubmissionAdmission,
    ) -> Result<(), BlkError> {
        if count == 0 {
            return Err(BlkError::InvalidRequest);
        }
        loop {
            while self.flush_active.load(Ordering::Acquire) {
                if admission.cannot_wait() {
                    return Err(BlkError::Retry);
                }
                self.barrier_notification.wait();
            }
            self.active_data
                .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                    active.checked_add(count)
                })
                .map_err(|_| BlkError::InvalidRequest)?;
            if !self.flush_active.load(Ordering::Acquire) {
                return Ok(());
            }
            self.active_data.fetch_sub(count, Ordering::AcqRel);
            self.barrier_notification.notify();
        }
    }

    fn begin_flush_barrier(&self, admission: SubmissionAdmission) -> Result<(), BlkError> {
        loop {
            if self
                .flush_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                break;
            }
            if admission.cannot_wait() {
                return Err(BlkError::Retry);
            }
            self.barrier_notification.wait();
        }
        while self.active_data.load(Ordering::Acquire) != 0 {
            if admission.cannot_wait() {
                self.flush_active.store(false, Ordering::Release);
                self.barrier_notification.notify();
                return Err(BlkError::Retry);
            }
            self.barrier_notification.wait();
        }
        Ok(())
    }

    fn undo_submission_admission(&self, op: RequestOp, count: usize) {
        match op {
            RequestOp::Read | RequestOp::Write => {
                self.active_data.fetch_sub(count, Ordering::AcqRel);
                self.barrier_notification.notify();
            }
            RequestOp::Flush => {
                self.flush_active.store(false, Ordering::Release);
                self.barrier_notification.notify();
            }
        }
    }

    fn install_update(
        self: &Arc<Self>,
        update: &mut ControllerUpdate,
        controller: Arc<ControllerPort>,
    ) -> Result<Vec<usize>, BlkError> {
        let queues = update.take_queues();
        let endpoints = update.take_irq_endpoints();
        if let Some(info) = update.take_device_info() {
            *self.info.lock() = info;
        }
        let online_cpus = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .online_cpu_count()
            .max(1);
        let existing = self.hctxs.lock().clone();
        let observer: Arc<dyn HctxObserver> = self.clone();
        let observer = Arc::downgrade(&observer);
        let event_port: Arc<dyn ControllerEventPort> = controller.clone();
        let mut new_hctxs = Vec::new();

        for queue in queues {
            let queue_id = queue.id();
            if existing
                .iter()
                .chain(new_hctxs.iter())
                .any(|hctx| hctx.id() == queue_id)
            {
                stop_hctxs(&new_hctxs);
                return Err(BlkError::InvalidRequest);
            }
            let cpu = (existing.len() + new_hctxs.len()) % online_cpus;
            match Hctx::start(queue, cpu, observer.clone(), Arc::clone(&event_port)) {
                Ok(hctx) => new_hctxs.push(hctx),
                Err(error) => {
                    stop_hctxs(&new_hctxs);
                    return Err(error);
                }
            }
        }

        let mut candidates = existing;
        candidates.extend(new_hctxs.iter().cloned());
        let mut new_registrations = Vec::new();
        let mut rearm_sources = Vec::new();
        for endpoint in endpoints {
            rearm_sources.push(endpoint.source_id());
            match self.register_endpoint(endpoint, &candidates) {
                Ok(registration) => new_registrations.push(registration),
                Err(error) => {
                    disable_registrations(&new_registrations);
                    stop_hctxs(&new_hctxs);
                    return Err(error);
                }
            }
        }
        for registration in &new_registrations {
            if registration.enable().is_err() {
                disable_registrations(&new_registrations);
                stop_hctxs(&new_hctxs);
                return Err(BlkError::Io);
            }
        }
        let rebuild_cpu_channels = !candidates.is_empty()
            && (!new_hctxs.is_empty() || self.cpu_channels.lock().len() < online_cpus);
        let new_cpu_channels = if rebuild_cpu_channels {
            match create_cpu_channels(&candidates, online_cpus) {
                Ok(channels) => Some(channels),
                Err(error) => {
                    disable_registrations(&new_registrations);
                    stop_hctxs(&new_hctxs);
                    return Err(error);
                }
            }
        } else {
            None
        };
        self.hctxs.lock().extend(new_hctxs);
        if let Some(new_cpu_channels) = new_cpu_channels {
            let old_channels = core::mem::replace(&mut *self.cpu_channels.lock(), new_cpu_channels);
            for channel in old_channels {
                channel.channel.close();
            }
        }
        self.irq_registrations.lock().extend(new_registrations);
        if update.controller_state() == ControllerState::Ready && !self.hctxs.lock().is_empty() {
            self.state.store(DEVICE_READY, Ordering::Release);
            self.accepting.store(true, Ordering::Release);
            self.state_notification.notify();
        }
        Ok(rearm_sources)
    }

    fn register_endpoint(
        &self,
        endpoint: IrqEndpoint,
        hctxs: &[Arc<Hctx>],
    ) -> Result<Box<dyn BlockIrqRegistration>, BlkError> {
        let source_id = endpoint.source_id();
        let queue_bits = endpoint.queue_bits();
        let mut targets: Vec<IrqTarget> = Vec::new();
        for hctx in hctxs {
            if queue_bits & (1u64 << hctx.id()) != 0 {
                targets.push(hctx.irq_target(source_id));
            }
        }
        let cpu = targets
            .first()
            .and_then(|_| {
                hctxs
                    .iter()
                    .find(|hctx| queue_bits & (1u64 << hctx.id()) != 0)
                    .map(|hctx| hctx.cpu())
            })
            .unwrap_or(0);
        let irq = self
            .irq_sources
            .iter()
            .find(|source| source.source_id == source_id)
            .map(|source| source.irq)
            .ok_or(BlkError::NotSupported)?;
        let controller_target = self.controller.irq_target(source_id);
        let registration = register_block_irq(
            format!("{}/irq-{source_id}", self.name),
            irq,
            cpu,
            BlockIrqAction::new(endpoint.into_handler(), targets)
                .with_controller_target(controller_target),
        )
        .map_err(|_| BlkError::Io)?;
        info!(
            "block device {} IRQ source {} ({irq:?}) fixed to CPU {} for queue mask \
             {queue_bits:#x}",
            self.name, source_id, cpu
        );
        Ok(registration)
    }

    fn wait_until_ready(&self, timeout: Duration) -> Result<(), BlkError> {
        let deadline = wall_time().saturating_add(timeout);
        loop {
            match self.state.load(Ordering::Acquire) {
                DEVICE_READY => return Ok(()),
                DEVICE_FAILED | DEVICE_STOPPED => return Err(BlkError::Io),
                _ => {}
            }
            let now = wall_time();
            if now >= deadline {
                self.state.store(DEVICE_FAILED, Ordering::Release);
                return Err(BlkError::Io);
            }
            if self.state_notification.wait_timeout(deadline - now)
                && self.state.load(Ordering::Acquire) != DEVICE_READY
            {
                self.state.store(DEVICE_FAILED, Ordering::Release);
                return Err(BlkError::Io);
            }
        }
    }

    fn online_smp(&self) -> Result<(), BlkError> {
        if self.state.load(Ordering::Acquire) != DEVICE_READY {
            return Err(BlkError::Io);
        }
        let cpus = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .online_cpu_count()
            .max(1);
        let target = cpus.min(self.max_io_queues).min(MAX_RUNTIME_HCTX);
        let current = self.hctxs.lock().len();
        if target <= current {
            self.ensure_cpu_channels(cpus)?;
            info!(
                "block device {} online with {} hctxs across {} CPUs",
                self.name, current, cpus
            );
            return Ok(());
        }
        let state = self.controller.call(ControllerEvent::OnlineSmp {
            target_queues: target,
        })?;
        if state == ControllerState::Ready && self.hctxs.lock().len() >= target {
            info!(
                "block device {} online with {} hctxs across {} CPUs",
                self.name,
                self.hctxs.lock().len(),
                cpus
            );
            Ok(())
        } else {
            Err(BlkError::Io)
        }
    }

    fn ensure_cpu_channels(&self, online_cpus: usize) -> Result<(), BlkError> {
        if self.cpu_channels.lock().len() >= online_cpus {
            return Ok(());
        }
        let hctxs = self.hctxs.lock().clone();
        let new_channels = create_cpu_channels(&hctxs, online_cpus)?;
        let old_channels = core::mem::replace(&mut *self.cpu_channels.lock(), new_channels);
        for channel in old_channels {
            channel.channel.close();
        }
        Ok(())
    }

    fn shutdown(&self) -> usize {
        let previous = self.state.swap(DEVICE_STOPPED, Ordering::AcqRel);
        if previous == DEVICE_STOPPED {
            return 0;
        }
        self.accepting.store(false, Ordering::Release);
        self.barrier_notification.notify();

        let _ = self.controller.call(ControllerEvent::QuiesceIrqs);
        let registrations = core::mem::take(&mut *self.irq_registrations.lock());
        let count = registrations.len();
        disable_registrations(&registrations);
        drop(registrations);

        let hctxs = core::mem::take(&mut *self.hctxs.lock());
        let cpu_channels = core::mem::take(&mut *self.cpu_channels.lock());
        for channel in cpu_channels {
            channel.channel.close();
        }
        stop_hctxs(&hctxs);
        let _ = self.controller.call(ControllerEvent::Shutdown);
        self.controller.commands.close();
        // Drop the IRQ-disabling slot guard before `join`, which may sleep.
        let thread = self.controller_thread.lock().take();
        if let Some(thread) = thread {
            thread.join();
        }
        count
    }
}

#[derive(Clone)]
struct CpuSubmissionChannel {
    hctx: Arc<Hctx>,
    channel: Arc<BoundedChannel<Submission>>,
}

fn create_cpu_channels(
    hctxs: &[Arc<Hctx>],
    online_cpus: usize,
) -> Result<Vec<CpuSubmissionChannel>, BlkError> {
    if hctxs.is_empty() || online_cpus == 0 {
        return Err(BlkError::InvalidRequest);
    }
    let mut channels = Vec::with_capacity(online_cpus);
    for cpu in 0..online_cpus {
        let hctx = Arc::clone(&hctxs[cpu % hctxs.len()]);
        let channel = hctx.add_submission_channel()?;
        channels.push(CpuSubmissionChannel { hctx, channel });
    }
    Ok(channels)
}

impl HctxObserver for DeviceInner {
    fn request_completed(&self, op: RequestOp, block_count: u32, result: Result<(), BlkError>) {
        match op {
            RequestOp::Read => {
                self.active_data.fetch_sub(1, Ordering::AcqRel);
                if result.is_ok() {
                    BLOCK_READS.fetch_add(1, Ordering::Relaxed);
                    BLOCK_SECTORS_READ.fetch_add(
                        sectors_for_blocks(self.info.lock().logical_block_size, block_count),
                        Ordering::Relaxed,
                    );
                }
                self.barrier_notification.notify();
            }
            RequestOp::Write => {
                self.active_data.fetch_sub(1, Ordering::AcqRel);
                if result.is_ok() {
                    BLOCK_WRITES.fetch_add(1, Ordering::Relaxed);
                    BLOCK_SECTORS_WRITTEN.fetch_add(
                        sectors_for_blocks(self.info.lock().logical_block_size, block_count),
                        Ordering::Relaxed,
                    );
                }
                self.barrier_notification.notify();
            }
            RequestOp::Flush => {
                self.flush_active.store(false, Ordering::Release);
                self.barrier_notification.notify();
            }
        }
    }

    fn hctx_failed(&self, _hctx_id: usize, _error: BlkError) {
        self.accepting.store(false, Ordering::Release);
        self.state.store(DEVICE_FAILED, Ordering::Release);
        self.state_notification.notify();
        self.barrier_notification.notify();
    }
}

struct ControllerPort {
    commands: BoundedChannel<ControllerCommand>,
    notification: Arc<dyn BlockNotification>,
    irq_latches: IrqMutex<Vec<Arc<ControllerIrqLatch>>>,
}

struct ControllerCommand {
    event: ControllerEvent,
    reply: Option<ControllerReplySender>,
}

struct ControllerReply {
    result: IrqMutex<Option<Result<ControllerState, BlkError>>>,
    notification: Arc<dyn BlockNotification>,
}

struct ControllerReplySender {
    inner: Arc<ControllerReply>,
}

impl ControllerPort {
    fn call(&self, event: ControllerEvent) -> Result<ControllerState, BlkError> {
        let notification = runtime_ops()
            .map_err(|_| BlkError::Other("block runtime adapter is not installed"))?
            .notification();
        let reply = Arc::new(ControllerReply {
            result: IrqMutex::new(None),
            notification,
        });
        let command = ControllerCommand {
            event,
            reply: Some(ControllerReplySender {
                inner: Arc::clone(&reply),
            }),
        };
        self.commands
            .send(command, false)
            .map_err(|_| BlkError::Io)?;
        loop {
            if let Some(result) = reply.result.lock().take() {
                return result;
            }
            reply.notification.wait();
        }
    }

    fn irq_target(&self, source_id: usize) -> ControllerIrqTarget {
        let latch = Arc::new(ControllerIrqLatch::new(source_id));
        self.irq_latches.lock().push(Arc::clone(&latch));
        ControllerIrqTarget::new(latch, Arc::clone(&self.notification))
    }
}

impl ControllerEventPort for ControllerPort {
    fn post(&self, event: ControllerEvent) {
        if let Err(SendError::Closed(_) | SendError::Full(_)) = self
            .commands
            .send(ControllerCommand { event, reply: None }, false)
        {
            warn!("lost block controller event after controller shutdown");
        }
    }
}

impl ControllerReplySender {
    fn complete(self, result: Result<ControllerState, BlkError>) {
        *self.inner.result.lock() = Some(result);
        self.inner.notification.notify();
    }
}

fn run_controller(
    mut controller: Box<dyn BlockController>,
    port: Arc<ControllerPort>,
    device: Weak<DeviceInner>,
) {
    loop {
        let mut progressed = false;
        let latches = port.irq_latches.lock().clone();
        for latch in latches {
            let event = latch.take();
            if event.control.is_empty() {
                continue;
            }
            progressed = true;
            let _ = drive_controller_transition(
                &mut *controller,
                ControllerEvent::Irq(event.control),
                device.upgrade(),
                Arc::clone(&port),
            );
            if event.needs_rearm {
                let _ = drive_controller_transition(
                    &mut *controller,
                    ControllerEvent::Rearm {
                        source_id: event.control.source_id(),
                    },
                    device.upgrade(),
                    Arc::clone(&port),
                );
            }
        }

        while let Some(command) = port.commands.try_recv() {
            progressed = true;
            let result = drive_controller_transition(
                &mut *controller,
                command.event,
                device.upgrade(),
                Arc::clone(&port),
            );
            if let Some(reply) = command.reply {
                reply.complete(result);
            }
            if command.event == ControllerEvent::Shutdown {
                return;
            }
        }
        if !progressed {
            port.notification.wait();
        }
    }
}

fn drive_controller_transition(
    controller: &mut dyn BlockController,
    mut event: ControllerEvent,
    device: Option<Arc<DeviceInner>>,
    port: Arc<ControllerPort>,
) -> Result<ControllerState, BlkError> {
    let deadline = wall_time().saturating_add(CONTROLLER_TRANSITION_TIMEOUT);
    loop {
        let mut update = match controller.advance(event) {
            Ok(update) => update,
            Err(error) => {
                warn!("block controller transition {event:?} failed: {error:?}");
                return Err(error);
            }
        };
        let state = update.controller_state();
        if let Some(device) = &device {
            let rearm_sources = match device.install_update(&mut update, Arc::clone(&port)) {
                Ok(sources) => sources,
                Err(error) => {
                    warn!("failed to install block controller update after {event:?}: {error:?}");
                    device.state.store(DEVICE_FAILED, Ordering::Release);
                    device.state_notification.notify();
                    return Err(error);
                }
            };
            for source_id in rearm_sources {
                let mut rearm = match controller.advance(ControllerEvent::Rearm { source_id }) {
                    Ok(update) => update,
                    Err(error) => {
                        warn!("failed to rearm block IRQ source {source_id}: {error:?}");
                        return Err(error);
                    }
                };
                if let Err(error) = device.install_update(&mut rearm, Arc::clone(&port)) {
                    warn!(
                        "failed to install block controller rearm update for source {source_id}: \
                         {error:?}"
                    );
                    device.state.store(DEVICE_FAILED, Ordering::Release);
                    device.state_notification.notify();
                    return Err(error);
                }
            }
        }
        if state != ControllerState::RegisterPending {
            return Ok(state);
        }
        if wall_time() >= deadline {
            return Err(BlkError::Io);
        }
        core::hint::spin_loop();
        event = ControllerEvent::RegisterRetry;
    }
}

fn next_chunk(
    info: QueueInfo,
    base_lba: u64,
    total_len: usize,
    byte_offset: usize,
) -> Result<rdif_block::TransferChunk, BlkError> {
    let boundary_cap = info
        .limits
        .segment_boundary
        .unwrap_or(MAX_RUNTIME_TRANSFER_BYTES);
    let planner = TransferPlanner::new(
        info.device,
        info.limits,
        TransferRuntimeCaps::new(MAX_RUNTIME_TRANSFER_BYTES.min(boundary_cap), 1),
    )?;
    let lba_offset = byte_offset / info.device.logical_block_size;
    let lba = base_lba
        .checked_add(u64::try_from(lba_offset).map_err(|_| BlkError::InvalidRequest)?)
        .ok_or(BlkError::InvalidRequest)?;
    planner
        .plan_from(lba, total_len - byte_offset, byte_offset)?
        .next()
        .ok_or(BlkError::InvalidRequest)
}

fn request_cannot_block() -> bool {
    match runtime_ops() {
        Ok(ops) => !ops.can_block(),
        Err(_) => true,
    }
}

fn sectors_for_blocks(logical_block_size: usize, block_count: u32) -> u64 {
    (logical_block_size as u64)
        .saturating_mul(block_count as u64)
        .div_ceil(512)
}

fn stop_hctxs(hctxs: &[Arc<Hctx>]) {
    for hctx in hctxs {
        hctx.stop();
    }
}

fn disable_registrations(registrations: &[Box<dyn BlockIrqRegistration>]) {
    for registration in registrations {
        let _ = registration.disable_and_synchronize();
    }
}

/// Maps a portable block error at the filesystem integration boundary.
pub fn map_blk_err_to_ax_err(error: BlkError) -> AxError {
    match error {
        BlkError::NotSupported => AxError::Unsupported,
        BlkError::Retry => AxError::WouldBlock,
        BlkError::NoMemory => AxError::NoMemory,
        BlkError::InvalidBlockIndex(_) | BlkError::InvalidRequest => AxError::InvalidInput,
        BlkError::TimedOut => AxError::TimedOut,
        BlkError::Io | BlkError::Other(_) => AxError::Io,
    }
}

fn block_io_error(stage: &str, op: RequestOp, lba: u64, error: BlkError) -> AxError {
    warn!("block {op:?} at LBA {lba} failed during {stage}: {error:?}");
    map_blk_err_to_ax_err(error)
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, string::String, sync::Arc, vec, vec::Vec};
    use core::{
        any::Any,
        sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        time::Duration,
    };
    use std::{
        sync::{Mutex as StdMutex, mpsc},
        thread,
        time::Instant,
    };

    use irq_framework::{HwIrq, IrqDomainId, IrqId};
    use rdif_block::{
        BatchSubmitDisposition, BatchSubmitResult, CompletionSink, DriverGeneric, HardIrqHandler,
        HardwareQueue, IrqAck, OwnedRequestBatch, QueueLimits, SubmissionSink,
    };

    use super::*;
    use crate::os::{BlockIrqRegistrar, set_irq_registrar};

    struct LifecycleQueue {
        log: Arc<StdMutex<Vec<&'static str>>>,
    }

    impl HardwareQueue for LifecycleQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            test_queue_info()
        }

        fn submit_batch_owned(
            &mut self,
            _requests: &mut OwnedRequestBatch,
            _sink: &mut dyn SubmissionSink,
        ) -> BatchSubmitResult {
            BatchSubmitResult::new(0, BatchSubmitDisposition::QueueFull)
        }

        fn commit_submissions(&mut self) -> Result<(), BlkError> {
            Ok(())
        }

        fn drain_completions(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            Ok(())
        }

        fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
            self.log.lock().unwrap().push("queue_shutdown");
            Ok(())
        }
    }

    struct SpuriousHandler;

    impl HardIrqHandler for SpuriousHandler {
        fn ack(&mut self) -> IrqAck {
            IrqAck::spurious(0)
        }
    }

    struct LifecycleController {
        queue: Option<LifecycleQueue>,
        log: Arc<StdMutex<Vec<&'static str>>>,
    }

    impl DriverGeneric for LifecycleController {
        fn name(&self) -> &str {
            "lifecycle-controller"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl BlockController for LifecycleController {
        fn device_info(&self) -> DeviceInfo {
            test_queue_info().device
        }

        fn max_io_queues(&self) -> usize {
            1
        }

        fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
            match event {
                ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                    ControllerState::Ready,
                    vec![Box::new(self.queue.take().unwrap())],
                    vec![IrqEndpoint::new(0, 1, Box::new(SpuriousHandler))],
                )),
                ControllerEvent::QuiesceIrqs => {
                    self.log.lock().unwrap().push("controller_quiesce");
                    Ok(ControllerUpdate::state(ControllerState::Ready))
                }
                ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                    self.log.lock().unwrap().push("controller_shutdown");
                    Ok(ControllerUpdate::state(ControllerState::Shutdown))
                }
                _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
            }
        }
    }

    struct EndpointFirstController {
        queue: Option<LifecycleQueue>,
        register_retries: Arc<AtomicUsize>,
        log: Arc<StdMutex<Vec<&'static str>>>,
    }

    impl DriverGeneric for EndpointFirstController {
        fn name(&self) -> &str {
            "endpoint-first-controller"
        }

        fn raw_any(&self) -> Option<&dyn Any> {
            Some(self)
        }

        fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
            Some(self)
        }
    }

    impl BlockController for EndpointFirstController {
        fn device_info(&self) -> DeviceInfo {
            test_queue_info().device
        }

        fn max_io_queues(&self) -> usize {
            1
        }

        fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
            match event {
                ControllerEvent::Start { .. } => Ok(ControllerUpdate::with_resources(
                    ControllerState::RegisterPending,
                    Vec::new(),
                    vec![IrqEndpoint::new(0, 0, Box::new(SpuriousHandler))],
                )),
                ControllerEvent::RegisterRetry => {
                    self.register_retries.fetch_add(1, Ordering::Relaxed);
                    Ok(ControllerUpdate::with_resources(
                        ControllerState::Ready,
                        vec![Box::new(self.queue.take().unwrap())],
                        Vec::new(),
                    ))
                }
                ControllerEvent::Rearm { .. } => {
                    Ok(ControllerUpdate::state(ControllerState::RegisterPending))
                }
                ControllerEvent::QuiesceIrqs => {
                    self.log.lock().unwrap().push("controller_quiesce");
                    Ok(ControllerUpdate::state(ControllerState::Ready))
                }
                ControllerEvent::Shutdown | ControllerEvent::Watchdog { .. } => {
                    self.log.lock().unwrap().push("controller_shutdown");
                    Ok(ControllerUpdate::state(ControllerState::Shutdown))
                }
                _ => Ok(ControllerUpdate::state(ControllerState::Ready)),
            }
        }
    }

    struct TestIrqRegistrar {
        log: StdMutex<Option<Arc<StdMutex<Vec<&'static str>>>>>,
    }

    static TEST_IRQ_REGISTRAR: TestIrqRegistrar = TestIrqRegistrar {
        log: StdMutex::new(None),
    };
    static TEST_IRQ_REGISTRAR_SERIAL: StdMutex<()> = StdMutex::new(());

    fn lock_test_irq_registrar() -> std::sync::MutexGuard<'static, ()> {
        TEST_IRQ_REGISTRAR_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct TestIrqRegistration {
        log: Arc<StdMutex<Vec<&'static str>>>,
        _action: StdMutex<Option<BlockIrqAction>>,
    }

    impl BlockIrqRegistration for TestIrqRegistration {
        fn enable(&self) -> AxResult {
            self.log.lock().unwrap().push("irq_enable");
            Ok(())
        }

        fn disable_and_synchronize(&self) -> AxResult {
            self.log.lock().unwrap().push("irq_disable_sync");
            Ok(())
        }
    }

    impl Drop for TestIrqRegistration {
        fn drop(&mut self) {
            self.log.lock().unwrap().push("irq_free");
        }
    }

    impl BlockIrqRegistrar for TestIrqRegistrar {
        fn register(
            &self,
            _name: String,
            _irq: IrqId,
            _cpu: usize,
            action: BlockIrqAction,
        ) -> AxResult<Box<dyn BlockIrqRegistration>> {
            let log = self.log.lock().unwrap().clone().ok_or(AxError::BadState)?;
            log.lock().unwrap().push("irq_register_disabled");
            Ok(Box::new(TestIrqRegistration {
                log,
                _action: StdMutex::new(Some(action)),
            }))
        }
    }

    fn test_queue_info() -> QueueInfo {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.max_inflight = 1;
        limits.supports_flush = true;
        QueueInfo {
            id: 0,
            device: DeviceInfo::new(32, 512),
            limits,
        }
    }

    fn log_position(log: &[&str], item: &str) -> usize {
        log.iter()
            .position(|entry| *entry == item)
            .unwrap_or_else(|| panic!("missing lifecycle event {item}: {log:?}"))
    }

    #[test]
    fn teardown_masks_and_synchronizes_before_queue_and_controller_shutdown() {
        let _registrar_guard = lock_test_irq_registrar();
        crate::os::task::install_test_runtime_ops();
        let log = Arc::new(StdMutex::new(Vec::new()));
        *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
        set_irq_registrar(&TEST_IRQ_REGISTRAR);

        let controller = LifecycleController {
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
            log: Arc::clone(&log),
        };
        let irq = IrqId::new(IrqDomainId(1), HwIrq(9));
        let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
            "lifecycle",
            [BlockIrqSource { source_id: 0, irq }],
            Box::new(controller),
        ))
        .unwrap();

        let hctxs = handle.inner.hctxs.lock().clone();
        let cpu_channels = create_cpu_channels(&hctxs, 8).unwrap();
        assert_eq!(cpu_channels.len(), 8);
        assert!(cpu_channels.iter().all(|channel| channel.hctx.id() == 0));
        for channel in cpu_channels {
            channel.channel.close();
        }

        assert_eq!(handle.shutdown(), 1);
        let log = log.lock().unwrap();
        let quiesce = log_position(&log, "controller_quiesce");
        let disable = log_position(&log, "irq_disable_sync");
        let free = log_position(&log, "irq_free");
        let queue = log_position(&log, "queue_shutdown");
        let controller = log_position(&log, "controller_shutdown");
        assert!(quiesce < disable);
        assert!(disable < free);
        assert!(free < queue);
        assert!(queue < controller);
    }

    #[test]
    fn controller_can_register_control_irq_before_creating_an_io_queue() {
        let _registrar_guard = lock_test_irq_registrar();
        crate::os::task::install_test_runtime_ops();
        let log = Arc::new(StdMutex::new(Vec::new()));
        *TEST_IRQ_REGISTRAR.log.lock().unwrap() = Some(Arc::clone(&log));
        set_irq_registrar(&TEST_IRQ_REGISTRAR);
        let register_retries = Arc::new(AtomicUsize::new(0));
        let controller = EndpointFirstController {
            queue: Some(LifecycleQueue {
                log: Arc::clone(&log),
            }),
            register_retries: Arc::clone(&register_retries),
            log,
        };
        let irq = IrqId::new(IrqDomainId(1), HwIrq(10));

        let handle = BlockDeviceHandle::start(RdifBlockDevice::new_with_irqs(
            "endpoint-first",
            [BlockIrqSource { source_id: 0, irq }],
            Box::new(controller),
        ))
        .unwrap();

        assert_eq!(register_retries.load(Ordering::Relaxed), 1);
        assert_eq!(handle.inner.hctxs.lock().len(), 1);
        assert_eq!(handle.inner.cpu_channels.lock().len(), 1);
        assert_eq!(handle.shutdown(), 1);
    }

    fn barrier_test_inner() -> Arc<DeviceInner> {
        let ops = runtime_ops().unwrap();
        let controller_notification = ops.notification();
        Arc::new(DeviceInner {
            name: String::from("barrier-test"),
            info: IrqMutex::new(test_queue_info().device),
            max_io_queues: 1,
            irq_sources: Vec::new(),
            hctxs: IrqMutex::new(Vec::new()),
            cpu_channels: IrqMutex::new(Vec::new()),
            irq_registrations: IrqMutex::new(Vec::new()),
            controller: Arc::new(ControllerPort {
                commands: BoundedChannel::with_item_notification(
                    1,
                    Arc::clone(&controller_notification),
                )
                .unwrap(),
                notification: controller_notification,
                irq_latches: IrqMutex::new(Vec::new()),
            }),
            controller_thread: IrqMutex::new(None),
            state: AtomicU8::new(DEVICE_READY),
            accepting: AtomicBool::new(true),
            active_data: AtomicUsize::new(0),
            flush_active: AtomicBool::new(false),
            barrier_notification: ops.notification(),
            state_notification: ops.notification(),
        })
    }

    #[test]
    fn flush_barrier_waits_for_prior_data_and_holds_later_data() {
        crate::os::task::install_test_runtime_ops();
        let inner = barrier_test_inner();
        inner
            .enter_data_submissions(1, SubmissionAdmission::Blocking)
            .unwrap();

        let flush_inner = Arc::clone(&inner);
        let (flush_tx, flush_rx) = mpsc::channel();
        let flush_thread = thread::spawn(move || {
            flush_inner
                .begin_flush_barrier(SubmissionAdmission::Blocking)
                .unwrap();
            flush_tx.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !inner.flush_active.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "flush gate was not acquired");
            thread::yield_now();
        }

        let later_inner = Arc::clone(&inner);
        let (later_tx, later_rx) = mpsc::channel();
        let later_thread = thread::spawn(move || {
            later_inner
                .enter_data_submissions(1, SubmissionAdmission::Blocking)
                .unwrap();
            later_tx.send(()).unwrap();
        });
        assert!(flush_rx.recv_timeout(Duration::from_millis(20)).is_err());
        assert!(later_rx.recv_timeout(Duration::from_millis(20)).is_err());

        inner.request_completed(RequestOp::Write, 1, Ok(()));
        flush_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(later_rx.recv_timeout(Duration::from_millis(20)).is_err());

        inner.request_completed(RequestOp::Flush, 0, Ok(()));
        later_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        inner.request_completed(RequestOp::Read, 1, Ok(()));
        flush_thread.join().unwrap();
        later_thread.join().unwrap();
    }

    #[test]
    fn nowait_admission_never_sleeps_behind_flush_barrier() {
        crate::os::task::install_test_runtime_ops();
        let inner = barrier_test_inner();
        inner.flush_active.store(true, Ordering::Release);

        assert_eq!(
            inner.enter_data_submissions(1, SubmissionAdmission::Nowait),
            Err(BlkError::Retry)
        );
        assert_eq!(inner.active_data.load(Ordering::Acquire), 0);

        inner.flush_active.store(false, Ordering::Release);
        inner.active_data.store(1, Ordering::Release);
        assert_eq!(
            inner.begin_flush_barrier(SubmissionAdmission::Nowait),
            Err(BlkError::Retry)
        );
        assert!(!inner.flush_active.load(Ordering::Acquire));
    }
}
