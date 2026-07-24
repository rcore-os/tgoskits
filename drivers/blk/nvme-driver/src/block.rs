use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    any::Any,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::{CoherentArray, InFlightDma};
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

const MAX_PRP_LIST_PAGES: usize = 1;
const DEFAULT_QUEUE_DEPTH: usize = 64;
const CONTROL_EVENT_ADMIN: u64 = 1;

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
            NvmeInitProgress::RegisterPending => ControllerState::RegisterPending,
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
                Ok(ControllerUpdate::state(ControllerState::RegisterPending))
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
        }
        Ok(ControllerUpdate::state(ControllerState::Shutdown))
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

struct NvmeBlockQueue {
    id: usize,
    name: &'static str,
    namespace: Namespace,
    dma_mask: u64,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    depth: usize,
    queue: NvmeQueue,
    state: NvmeQueueState,
}

struct NvmeQueueState {
    slots: Vec<RequestSlot>,
    free_cids: Vec<usize>,
    free_prp_lists: Vec<CoherentArray<u64>>,
}

struct RequestSlot {
    pending: bool,
    prp_list: Option<CoherentArray<u64>>,
    dma: Option<InFlightDma>,
}

struct PrpMapping {
    prp1: u64,
    prp2: u64,
    prp_list: Option<CoherentArray<u64>>,
}

impl NvmeBlockQueue {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: usize,
        depth: usize,
        name: &'static str,
        namespace: Namespace,
        dma_mask: u64,
        page_size: usize,
        max_transfer_bytes: Option<usize>,
        queue: NvmeQueue,
        prp_lists: Vec<CoherentArray<u64>>,
    ) -> Self {
        let mut slots = Vec::with_capacity(depth + 1);
        slots.resize_with(depth + 1, || RequestSlot {
            pending: false,
            prp_list: None,
            dma: None,
        });
        Self {
            id,
            name,
            namespace,
            dma_mask,
            page_size,
            max_transfer_bytes,
            depth,
            queue,
            state: NvmeQueueState {
                slots,
                free_cids: (1..=depth).rev().collect(),
                free_prp_lists: prp_lists,
            },
        }
    }

    fn queue_info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: device_info(self.name, self.namespace),
            limits: limits(
                self.dma_mask,
                self.page_size,
                self.max_transfer_bytes,
                self.namespace,
                self.depth,
            ),
        }
    }

    fn complete_one(&mut self, completion: NvmeCompletion, sink: &mut dyn CompletionSink) {
        let cid = usize::from(completion.command_id);
        let Some(slot) = self.state.slots.get_mut(cid) else {
            warn!(
                "nvme queue {} returned out-of-range command id {}",
                self.id, cid
            );
            return;
        };
        if !slot.pending {
            warn!(
                "nvme queue {} returned completion for free command id {}",
                self.id, cid
            );
            return;
        }

        let result = if completion.status.is_success() {
            Ok(())
        } else {
            warn!(
                "nvme queue {} request {} failed: status={:#x}, result={:#x}",
                self.id, cid, completion.status.0, completion.result
            );
            Err(BlkError::Io)
        };
        let dma = slot.dma.take().map(|dma| {
            // SAFETY: consuming a CQ entry and advancing the CQ head is the
            // controller's terminal ownership handoff for this command id.
            unsafe { dma.complete_after_quiesce() }
        });
        if let Some(prp_list) = slot.prp_list.take() {
            self.state.free_prp_lists.push(prp_list);
        }
        slot.pending = false;
        self.state.free_cids.push(cid);
        sink.complete(CompletedRequest::new(RequestId::new(cid), result, dma));
    }

    fn stage_next(&mut self, requests: &mut OwnedRequestBatch) -> Result<RequestId, BlkError> {
        let Some(mut request) = requests.pop_front() else {
            return Err(BlkError::InvalidRequest);
        };
        if let Err(error) = validate_owned_request(self.queue_info(), &request) {
            requests.push_front(request);
            return Err(error);
        }

        let Some(cid) = self.state.free_cids.pop() else {
            requests.push_front(request);
            return Err(BlkError::Retry);
        };
        let mapping = match self
            .state
            .build_command(self.namespace, self.page_size, cid, &request)
        {
            Ok(mapping) => mapping,
            Err(error) => {
                self.state.free_cids.push(cid);
                requests.push_front(request);
                return Err(error);
            }
        };

        let dma = request.data.take().map(|dma| {
            // SAFETY: the backing is installed in the command slot before the
            // batch commit transfers ownership to the controller.
            unsafe { dma.into_in_flight() }
        });
        let slot = &mut self.state.slots[cid];
        slot.pending = true;
        slot.prp_list = mapping.prp_list;
        slot.dma = dma;
        self.queue.stage_io_data(mapping.command);
        Ok(RequestId::new(cid))
    }
}

impl HardwareQueue for NvmeBlockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        self.queue_info()
    }

    fn submit_batch_owned(
        &mut self,
        requests: &mut OwnedRequestBatch,
        sink: &mut dyn SubmissionSink,
    ) -> BatchSubmitResult {
        let mut accepted = 0;
        let limit = self.queue_info().limits.max_submit_batch;
        while accepted < limit && !requests.is_empty() {
            match self.stage_next(requests) {
                Ok(id) => {
                    sink.accepted(id);
                    accepted += 1;
                }
                Err(BlkError::Retry) => {
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::QueueFull);
                }
                Err(error) => {
                    return BatchSubmitResult::new(accepted, BatchSubmitDisposition::Fatal(error));
                }
            }
        }
        BatchSubmitResult::new(accepted, BatchSubmitDisposition::Continue)
    }

    fn commit_submissions(&mut self) -> Result<(), BlkError> {
        self.queue.commit_io_submissions();
        Ok(())
    }

    fn drain_completions(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        let mut drained = false;
        while let Some(completion) = self.queue.take_completion_after_irq() {
            drained = true;
            self.complete_one(completion, sink);
        }
        if drained {
            self.queue.commit_completion_head();
        }
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        for cid in 1..self.state.slots.len() {
            let slot = &mut self.state.slots[cid];
            if !slot.pending {
                continue;
            }
            if let Some(dma) = slot.dma.take() {
                // `QuarantinedDma` deliberately leaks its backing when it
                // leaves scope. Keep the typed value visible here so teardown
                // cannot accidentally look like a normal DMA completion.
                let _quarantined = dma.quarantine();
            }
            if let Some(prp_list) = slot.prp_list.take() {
                self.state.free_prp_lists.push(prp_list);
            }
            slot.pending = false;
            sink.complete(CompletedRequest::new(
                RequestId::new(cid),
                Err(BlkError::Io),
                None,
            ));
        }
        self.state.free_cids = (1..=self.depth).rev().collect();
        Ok(())
    }
}

struct BuiltCommand {
    command: CommandSet,
    prp_list: Option<CoherentArray<u64>>,
}

impl NvmeQueueState {
    fn build_command(
        &mut self,
        namespace: Namespace,
        page_size: usize,
        cid: usize,
        request: &OwnedRequest,
    ) -> Result<BuiltCommand, BlkError> {
        let cid = u16::try_from(cid).map_err(|_| BlkError::InvalidRequest)?;
        match request.op {
            RequestOp::Read | RequestOp::Write => {
                let prp = self.build_prp_mapping(page_size, request)?;
                let command = match request.op {
                    RequestOp::Read => CommandSet::nvm_cmd_read_with_cid(
                        namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    RequestOp::Write => CommandSet::nvm_cmd_write_with_cid(
                        namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    _ => unreachable!(),
                };
                Ok(BuiltCommand {
                    command,
                    prp_list: prp.prp_list,
                })
            }
            RequestOp::Flush => Ok(BuiltCommand {
                command: CommandSet::nvm_cmd_flush_with_cid(namespace.id, cid),
                prp_list: None,
            }),
        }
    }

    fn build_prp_mapping(
        &mut self,
        page_size: usize,
        request: &OwnedRequest,
    ) -> Result<PrpMapping, BlkError> {
        let data = request.data.as_ref().ok_or(BlkError::InvalidRequest)?;
        let mut prps = PrpPageAccumulator::new();
        for segment in data.segments() {
            prps.push_segment(segment.addr.as_u64(), segment.len.get(), page_size)?;
        }
        let pages = prps.into_pages();
        let prp1 = *pages.first().ok_or(BlkError::InvalidRequest)?;
        let prp2 = match pages.len() {
            1 => 0,
            2 => pages[1],
            _ => {
                let list_entries = page_size / core::mem::size_of::<u64>();
                if pages.len() - 1 > list_entries * MAX_PRP_LIST_PAGES {
                    return Err(BlkError::InvalidRequest);
                }
                let mut list = self.free_prp_lists.pop().ok_or(BlkError::Retry)?;
                for entry in 0..list_entries {
                    list.set_cpu(entry, 0);
                }
                for (entry, addr) in pages[1..].iter().copied().enumerate() {
                    list.set_cpu(entry, addr);
                }
                let addr = list.dma_addr().as_u64();
                return Ok(PrpMapping {
                    prp1,
                    prp2: addr,
                    prp_list: Some(list),
                });
            }
        };
        Ok(PrpMapping {
            prp1,
            prp2,
            prp_list: None,
        })
    }
}

fn alloc_prp_lists(nvme: &Nvme, depth: usize) -> NvmeResult<Vec<CoherentArray<u64>>> {
    let mut lists = Vec::with_capacity(depth);
    for _ in 0..depth {
        lists.push(nvme.alloc_prp_list()?);
    }
    Ok(lists)
}

#[derive(Default)]
struct PrpPageAccumulator {
    pages: Vec<u64>,
    last_end: Option<u64>,
    current_page_end: Option<u64>,
}

impl PrpPageAccumulator {
    const fn new() -> Self {
        Self {
            pages: Vec::new(),
            last_end: None,
            current_page_end: None,
        }
    }

    fn into_pages(self) -> Vec<u64> {
        self.pages
    }

    fn push_segment(&mut self, addr: u64, len: usize, page_size: usize) -> Result<(), BlkError> {
        if page_size == 0 || len == 0 {
            return Err(BlkError::InvalidRequest);
        }
        let page_size = u64::try_from(page_size).map_err(|_| BlkError::InvalidRequest)?;
        let end = addr
            .checked_add(u64::try_from(len).map_err(|_| BlkError::InvalidRequest)?)
            .ok_or(BlkError::InvalidRequest)?;
        let mut cursor = addr;

        while cursor < end {
            self.ensure_page_entry(cursor, page_size)?;
            let page_end = self.current_page_end.ok_or(BlkError::InvalidRequest)?;
            let chunk_end = page_end.min(end);
            if chunk_end <= cursor {
                return Err(BlkError::InvalidRequest);
            }
            cursor = chunk_end;
            self.last_end = Some(cursor);
        }
        Ok(())
    }

    fn ensure_page_entry(&mut self, cursor: u64, page_size: u64) -> Result<(), BlkError> {
        let Some(last_end) = self.last_end else {
            self.push_page(cursor, page_size)?;
            return Ok(());
        };
        let current_page_end = self.current_page_end.ok_or(BlkError::InvalidRequest)?;
        if cursor < last_end {
            return Err(BlkError::InvalidRequest);
        }
        if cursor == last_end && cursor < current_page_end {
            return Ok(());
        }
        if cursor != last_end && last_end != current_page_end {
            return Err(BlkError::InvalidRequest);
        }
        if !cursor.is_multiple_of(page_size) {
            return Err(BlkError::InvalidRequest);
        }
        self.push_page(cursor, page_size)
    }

    fn push_page(&mut self, addr: u64, page_size: u64) -> Result<(), BlkError> {
        let page_base = addr / page_size * page_size;
        let page_end = page_base
            .checked_add(page_size)
            .ok_or(BlkError::InvalidRequest)?;
        self.pages.push(addr);
        self.current_page_end = Some(page_end);
        Ok(())
    }
}

fn device_info(name: &'static str, namespace: Namespace) -> DeviceInfo {
    DeviceInfo {
        name: Some(name),
        model: Some("nvme"),
        ..DeviceInfo::new(namespace.lba_count as u64, namespace.lba_size)
    }
}

fn limits(
    dma_mask: u64,
    page_size: usize,
    controller_max_transfer_bytes: Option<usize>,
    namespace: Namespace,
    max_inflight: usize,
) -> QueueLimits {
    let lba_size = namespace.lba_size.max(1);
    let prp_entries = page_size / core::mem::size_of::<u64>();
    let prp_capacity_bytes = page_size.saturating_mul(prp_entries + 1);
    let max_bytes = controller_max_transfer_bytes
        .map_or(prp_capacity_bytes, |max_transfer| {
            prp_capacity_bytes.min(max_transfer)
        })
        .max(lba_size);
    let max_blocks = max_bytes
        .checked_div(lba_size)
        .unwrap_or(1)
        .max(1)
        .min(u16::MAX as usize + 1) as u32;
    let max_bytes = (max_blocks as usize).saturating_mul(lba_size);
    QueueLimits {
        dma_mask,
        dma_domain: dma_api::DmaDomainId::legacy_global(),
        dma_alignment: lba_size,
        dma_length_alignment: lba_size,
        segment_boundary: None,
        max_inflight: max_inflight.max(1),
        max_submit_batch: max_inflight.max(1),
        max_blocks_per_request: max_blocks,
        max_segments: 1,
        max_segment_size: max_bytes,
        supported_flags: RequestFlags::NONE,
        supports_flush: true,
    }
}

#[cfg(test)]
mod tests {
    use super::{PrpPageAccumulator, limits};
    use crate::Namespace;

    #[test]
    fn queue_limits_enforce_lba_alignment_and_controller_transfer_limit() {
        let namespace = Namespace {
            id: 1,
            lba_size: 512,
            lba_count: 1024,
            metadata_size: 0,
        };
        let limits = limits(u64::MAX, 4096, Some(512 * 1024), namespace, 8);

        assert_eq!(limits.dma_alignment, 512);
        assert_eq!(limits.dma_length_alignment, 512);
        assert_eq!(limits.max_blocks_per_request, 1024);
        assert_eq!(limits.max_segment_size, 512 * 1024);
        assert_eq!(limits.max_segments, 1);
        assert_eq!(limits.max_submit_batch, 8);
        assert!(limits.supports_flush);
    }

    #[test]
    fn prp_pages_split_at_controller_page_boundaries() {
        let mut pages = PrpPageAccumulator::new();

        pages.push_segment(0x1800, 4096, 4096).unwrap();

        assert_eq!(pages.into_pages(), [0x1800, 0x2000]);
    }

    #[test]
    fn prp_pages_coalesce_contiguous_split_segments() {
        let mut pages = PrpPageAccumulator::new();

        pages.push_segment(0x1000, 4096, 4096).unwrap();
        pages.push_segment(0x2000, 2048, 4096).unwrap();
        pages.push_segment(0x2800, 2048, 4096).unwrap();

        assert_eq!(pages.into_pages(), [0x1000, 0x2000]);
    }

    #[test]
    fn prp_pages_reject_unaligned_non_contiguous_segment() {
        let mut pages = PrpPageAccumulator::new();

        pages.push_segment(0x1000, 2048, 4096).unwrap();

        assert!(pages.push_segment(0x2800, 512, 4096).is_err());
    }
}
