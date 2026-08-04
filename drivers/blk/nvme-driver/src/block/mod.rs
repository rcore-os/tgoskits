mod io_queue;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    any::Any,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use dma_api::{CoherentArray, InFlightDma};
use io_queue::{NvmeBlockQueue, alloc_prp_lists};
use log::warn;
use rdif_block::{
    BatchSubmitDisposition, BatchSubmitResult, BlkError, BlockController, CompletedRequest,
    CompletionSink, ControlEvent, ControllerEvent, ControllerState, ControllerUpdate, DeviceInfo,
    DriverGeneric, HardIrqHandler, HardwareQueue, IrqAck, IrqEndpoint, IrqQueueMask, OwnedRequest,
    OwnedRequestBatch, QueueInfo, QueueLimits, RequestFlags, RequestId, RequestOp, SubmissionSink,
    validate_owned_request,
};

use crate::{
    Namespace, Nvme,
    err::{Error as NvmeError, Result as NvmeResult},
    nvme::NvmeInitProgress,
    queue::{CommandSet, NvmeCompletion, NvmeQueue},
    registers::NvmeReg,
};

const DEFAULT_QUEUE_DEPTH: usize = 64;
const CONTROL_EVENT_ADMIN: u64 = 1;
const REGISTER_RETRY_DELAY: Duration = Duration::from_micros(100);

/// Fixed PCI INTx source test used before acknowledging a shared legacy line.
///
/// Implementations must be allocation-free and IRQ-safe. They may only inspect
/// the bound device's pre-resolved PCI configuration state.
pub trait NvmeIntxSource: Send + Sync + 'static {
    /// Returns whether this NVMe function currently asserts its INTx source.
    fn is_asserted(&self) -> bool;
}

/// Interrupt-driven NVMe controller exposed through `rdif-block`.
pub struct NvmeBlockDriver {
    name: &'static str,
    nvme: Nvme,
    namespace: Option<Namespace>,
    queue_depth: usize,
    bootstrap_target: usize,
    next_queue_id: usize,
    initialization_started: bool,
    ready: bool,
    stopped: bool,
    intx_source: Option<Arc<dyn NvmeIntxSource>>,
}

impl NvmeBlockDriver {
    /// Creates an interrupt-driven controller whose namespace is discovered by
    /// the controller maintenance task.
    pub fn from_nvme(nvme: Nvme) -> Self {
        Self::from_nvme_with_queue_depth(nvme, DEFAULT_QUEUE_DEPTH)
    }

    /// Creates a block controller with an explicit runtime queue depth.
    pub fn from_nvme_with_queue_depth(nvme: Nvme, queue_depth: usize) -> Self {
        Self {
            name: "nvme",
            nvme,
            namespace: None,
            queue_depth: queue_depth.max(1),
            bootstrap_target: 0,
            next_queue_id: 0,
            initialization_started: false,
            ready: false,
            stopped: false,
            intx_source: None,
        }
    }

    /// Installs the pre-resolved PCI INTx source test for legacy single-queue
    /// mode.
    pub fn with_intx_source(mut self, source: impl NvmeIntxSource) -> Self {
        self.intx_source = Some(Arc::new(source));
        self
    }

    fn initial_state(progress: &NvmeInitProgress) -> ControllerState {
        match progress {
            NvmeInitProgress::RegisterPending => ControllerState::RegisterPending {
                retry_after: REGISTER_RETRY_DELAY,
            },
            NvmeInitProgress::WaitingForIrq => ControllerState::WaitingForIrq,
            NvmeInitProgress::Ready(_) => ControllerState::Ready,
        }
    }

    fn apply_initialization_progress(
        &mut self,
        progress: NvmeInitProgress,
    ) -> Result<ControllerUpdate, BlkError> {
        match progress {
            NvmeInitProgress::RegisterPending => {
                Ok(ControllerUpdate::state(ControllerState::RegisterPending {
                    retry_after: REGISTER_RETRY_DELAY,
                }))
            }
            NvmeInitProgress::WaitingForIrq => {
                Ok(ControllerUpdate::state(ControllerState::WaitingForIrq))
            }
            NvmeInitProgress::Ready(namespace) => {
                if namespace.lba_size == 0
                    || namespace.lba_count == 0
                    || namespace.metadata_size != 0
                {
                    return Err(BlkError::NotSupported);
                }
                self.namespace = Some(namespace);
                self.ready = true;
                let info = device_info(self.name, namespace);
                Ok(self
                    .create_resources(self.bootstrap_target)?
                    .with_device_info(info))
            }
        }
    }

    fn admin_irq_endpoint(&self) -> IrqEndpoint {
        let source_id = self.nvme.admin_interrupt_source();
        IrqEndpoint::new(
            source_id,
            0,
            Box::new(NvmeAdminIrqHandler {
                registers: self.nvme.register_ptr(),
                source_id,
                intx: !self.nvme.msix_interrupts_enabled(),
                io_ready: self.nvme.intx_io_ready(),
                intx_source: self.intx_source.clone(),
            }),
        )
    }

    fn create_resources(&mut self, target_queues: usize) -> Result<ControllerUpdate, BlkError> {
        if self.stopped || !self.ready || target_queues == 0 {
            return Err(BlkError::NotSupported);
        }

        let target_queues = target_queues.min(self.max_io_queues());
        let mut queues: Vec<Box<dyn HardwareQueue>> = Vec::new();
        let mut endpoints = Vec::new();
        while self.next_queue_id < target_queues {
            let queue_id = self.next_queue_id;
            let queue = self.take_hardware_queue(queue_id)?;
            let endpoint = self.irq_endpoint(queue_id)?;
            self.next_queue_id += 1;
            queues.push(Box::new(queue));
            endpoints.push(endpoint);
        }
        Ok(ControllerUpdate::with_resources(
            ControllerState::Ready,
            queues,
            endpoints,
        ))
    }

    fn take_hardware_queue(&mut self, queue_id: usize) -> Result<NvmeBlockQueue, BlkError> {
        let namespace = self.namespace.ok_or(BlkError::InvalidRequest)?;
        let queue = self
            .nvme
            .take_io_queue(queue_id)
            .ok_or(BlkError::NotSupported)?;
        let depth = self.queue_depth.min(queue.depth().saturating_sub(1).max(1));
        let prp_lists = alloc_prp_lists(&self.nvme, depth).map_err(nvme_error_to_block)?;
        Ok(NvmeBlockQueue::new(
            queue_id,
            depth,
            self.name,
            namespace,
            self.nvme.dma_mask(),
            self.nvme.page_size(),
            self.nvme.max_transfer_bytes(),
            queue,
            prp_lists,
        ))
    }

    fn irq_endpoint(&self, queue_id: usize) -> Result<IrqEndpoint, BlkError> {
        let source_id = self
            .nvme
            .interrupt_source_for_io_queue(queue_id)
            .ok_or(BlkError::NotSupported)?;
        let queue_mask = IrqQueueMask::from_queue(queue_id);
        if queue_mask.is_empty() {
            return Err(BlkError::InvalidRequest);
        }
        let handler = NvmeBlockIrqHandler {
            registers: self.nvme.register_ptr(),
            source_id,
            queues: queue_mask,
            intx: !self.nvme.msix_interrupts_enabled(),
            io_ready: self.nvme.intx_io_ready(),
            intx_source: self.intx_source.clone(),
        };
        Ok(IrqEndpoint::new(
            source_id,
            queue_mask.bits(),
            Box::new(handler),
        ))
    }

    fn rearm_source(&mut self, source_id: usize) -> Result<(), BlkError> {
        if !self.initialization_started || self.stopped {
            return Err(BlkError::InvalidRequest);
        }
        self.nvme
            .unmask_interrupt_source(source_id)
            .map_err(nvme_error_to_block)
    }

    fn stop_controller(&mut self) -> Result<ControllerUpdate, BlkError> {
        if !self.stopped {
            self.nvme.mask_all_interrupt_sources();
            self.nvme.shutdown();
            self.stopped = true;
            self.ready = false;
        }
        Ok(ControllerUpdate::state(if self.nvme.shutdown_complete() {
            ControllerState::Shutdown
        } else {
            ControllerState::RegisterPending {
                retry_after: REGISTER_RETRY_DELAY,
            }
        }))
    }

    fn quiesce_interrupts(&mut self) -> ControllerUpdate {
        self.nvme.mask_all_interrupt_sources();
        ControllerUpdate::state(if self.stopped {
            ControllerState::Shutdown
        } else if self.ready {
            ControllerState::Ready
        } else {
            ControllerState::WaitingForIrq
        })
    }
}

impl DriverGeneric for NvmeBlockDriver {
    fn name(&self) -> &str {
        self.name
    }

    fn raw_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }
}

impl BlockController for NvmeBlockDriver {
    fn device_info(&self) -> DeviceInfo {
        self.namespace.map_or_else(
            || DeviceInfo {
                name: Some(self.name),
                model: Some("nvme"),
                ..DeviceInfo::new(0, 512)
            },
            |namespace| device_info(self.name, namespace),
        )
    }

    fn max_io_queues(&self) -> usize {
        self.nvme
            .configured_io_queue_count()
            .min(self.nvme.available_io_interrupt_sources())
            .min(u64::BITS as usize)
    }

    fn advance(&mut self, event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
        match event {
            ControllerEvent::Start { target_queues } => {
                if self.initialization_started || target_queues == 0 {
                    return Err(BlkError::InvalidRequest);
                }
                if !self.nvme.msix_interrupts_enabled() && self.intx_source.is_none() {
                    return Err(BlkError::NotSupported);
                }
                self.bootstrap_target = target_queues.min(self.max_io_queues());
                if self.bootstrap_target == 0 {
                    return Err(BlkError::NotSupported);
                }
                self.initialization_started = true;
                let progress = self
                    .nvme
                    .start_initialization()
                    .map_err(nvme_error_to_block)?;
                Ok(ControllerUpdate::with_resources(
                    Self::initial_state(&progress),
                    Vec::new(),
                    Vec::from([self.admin_irq_endpoint()]),
                ))
            }
            ControllerEvent::OnlineSmp { target_queues } => {
                if !self.ready {
                    return Err(BlkError::InvalidRequest);
                }
                self.create_resources(target_queues)
            }
            ControllerEvent::Irq(control) => {
                if control.source_id() != self.nvme.admin_interrupt_source()
                    || control.bits() & CONTROL_EVENT_ADMIN == 0
                {
                    return Ok(ControllerUpdate::state(if self.ready {
                        ControllerState::Ready
                    } else {
                        ControllerState::WaitingForIrq
                    }));
                }
                let progress = self.nvme.handle_admin_irq().map_err(nvme_error_to_block)?;
                self.apply_initialization_progress(progress)
            }
            ControllerEvent::RegisterRetry => {
                if self.stopped {
                    return self.stop_controller();
                }
                let progress = self
                    .nvme
                    .retry_initialization()
                    .map_err(nvme_error_to_block)?;
                self.apply_initialization_progress(progress)
            }
            ControllerEvent::Rearm { source_id } => {
                self.rearm_source(source_id)?;
                Ok(ControllerUpdate::state(ControllerState::Ready))
            }
            ControllerEvent::QuiesceIrqs => Ok(self.quiesce_interrupts()),
            ControllerEvent::Watchdog { .. } => self.stop_controller(),
            ControllerEvent::Shutdown => self.stop_controller(),
        }
    }
}

fn nvme_error_to_block(_error: NvmeError) -> BlkError {
    BlkError::Io
}

struct NvmeAdminIrqHandler {
    registers: NonNull<NvmeReg>,
    source_id: usize,
    intx: bool,
    io_ready: Arc<AtomicBool>,
    intx_source: Option<Arc<dyn NvmeIntxSource>>,
}

// SAFETY: The registration token is destroyed before the controller MMIO
// mapping. The handler accesses only fixed controller registers and an atomic
// phase flag; it cannot reach a queue, allocator, registry, filesystem, or
// scheduler object.
unsafe impl Send for NvmeAdminIrqHandler {}

impl HardIrqHandler for NvmeAdminIrqHandler {
    fn ack(&mut self) -> IrqAck {
        if self.intx {
            let asserted = self
                .intx_source
                .as_ref()
                .is_some_and(|source| source.is_asserted());
            if !asserted || self.io_ready.load(Ordering::Acquire) {
                return IrqAck::spurious(self.source_id);
            }
        }
        let control = ControlEvent::new(self.source_id, CONTROL_EVENT_ADMIN);
        if self.intx {
            let registers = unsafe { self.registers.as_ref() };
            registers.mask_interrupt_vector(0);
            IrqAck::masked_needs_rearm(IrqQueueMask::none(), control)
        } else {
            IrqAck::cleared(IrqQueueMask::none(), control)
        }
    }
}

struct NvmeBlockIrqHandler {
    registers: NonNull<NvmeReg>,
    source_id: usize,
    queues: IrqQueueMask,
    intx: bool,
    io_ready: Arc<AtomicBool>,
    intx_source: Option<Arc<dyn NvmeIntxSource>>,
}

// SAFETY: The registration token is destroyed before the controller MMIO
// mapping. The handler only performs fixed volatile register writes and owns no
// queue, allocator, registry, filesystem, or scheduler object.
unsafe impl Send for NvmeBlockIrqHandler {}

impl HardIrqHandler for NvmeBlockIrqHandler {
    fn ack(&mut self) -> IrqAck {
        if self.intx {
            let asserted = self
                .intx_source
                .as_ref()
                .is_some_and(|source| source.is_asserted());
            if !asserted || !self.io_ready.load(Ordering::Acquire) {
                return IrqAck::spurious(self.source_id);
            }
        }
        let control = ControlEvent::new(self.source_id, 0);
        if self.intx {
            let registers = unsafe { self.registers.as_ref() };
            registers.mask_interrupt_vector(0);
            IrqAck::masked_needs_rearm(self.queues, control)
        } else {
            IrqAck::cleared(self.queues, control)
        }
    }
}

fn device_info(name: &'static str, namespace: Namespace) -> DeviceInfo {
    DeviceInfo {
        name: Some(name),
        model: Some("nvme"),
        ..DeviceInfo::new(namespace.lba_count as u64, namespace.lba_size)
    }
}
