mod controller;
mod device;
mod io;

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
use controller::{ControllerPort, run_controller};
use device::CpuSubmissionChannel;
use irq_framework::IrqId;
#[cfg(feature = "ext4")]
use rdif_block::RequestFlags;
use rdif_block::{
    BatchSubmitError, BlkError, BlockController, ControllerEvent, ControllerState,
    ControllerUpdate, DeviceInfo, HardwareQueue, IrqEndpoint, OwnedRequest, OwnedRequestBatch,
    QueueInfo, RequestOp, SubmitError, validate_owned_request,
};
use spin::Once;

use super::{
    channel::{BoundedChannel, SendError},
    completion::{CompletionGroup, CompletionSubscription},
    hctx::{ControllerEventPort, Hctx, HctxObserver, Submission, request_is_nowait},
    irq::{
        BlockIrqAction, ControllerIrqLatch, ControllerIrqTarget, IrqTarget, LatchedControllerIrq,
    },
    waiters::TaskWaiters,
};
use crate::os::{
    BlockIrqRegistration, BlockNotification, BlockThread, register_block_irq, runtime_ops,
    sync::IrqMutex, wall_time,
};

const CONTROLLER_CHANNEL_DEPTH: usize = 64;
const CONTROLLER_TRANSITION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RUNTIME_HCTX: usize = u64::BITS as usize;

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
    detached_queues: IrqMutex<Vec<Box<dyn HardwareQueue>>>,
    cpu_channels: IrqMutex<Vec<CpuSubmissionChannel>>,
    irq_registrations: IrqMutex<Vec<Box<dyn BlockIrqRegistration>>>,
    controller: Arc<ControllerPort>,
    controller_thread: IrqMutex<Option<Box<dyn BlockThread>>>,
    state: AtomicU8,
    accepting: AtomicBool,
    active_data: AtomicUsize,
    flush_active: AtomicBool,
    data_gate_waiters: TaskWaiters,
    flush_gate_waiters: TaskWaiters,
    data_drain_waiters: TaskWaiters,
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
            detached_queues: IrqMutex::new(Vec::new()),
            cpu_channels: IrqMutex::new(Vec::new()),
            irq_registrations: IrqMutex::new(Vec::new()),
            controller: Arc::clone(&controller_port),
            controller_thread: IrqMutex::new(None),
            state: AtomicU8::new(DEVICE_STARTING),
            accepting: AtomicBool::new(false),
            active_data: AtomicUsize::new(0),
            flush_active: AtomicBool::new(false),
            data_gate_waiters: TaskWaiters::new(),
            flush_gate_waiters: TaskWaiters::new(),
            data_drain_waiters: TaskWaiters::new(),
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
        if !handle.inner.mark_ready() {
            handle.inner.shutdown();
            return Err(BlkError::Io);
        }
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
        io::read_blocks(self, block_id, buf)
    }

    #[cfg(any(feature = "ext4", feature = "fat"))]
    pub(crate) fn write_blocks(&self, block_id: u64, buf: &[u8]) -> AxResult {
        io::write_blocks(self, block_id, buf)
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

fn quiesce_hctxs(hctxs: &[Arc<Hctx>]) {
    for hctx in hctxs {
        hctx.quiesce();
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
mod tests;
