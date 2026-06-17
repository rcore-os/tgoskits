use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use dma_api::CoherentArray;
use rdif_block::{
    BlkError, DeviceInfo, DriverGeneric, Event, IQueue, IdList, Interface, IrqHandler,
    IrqSourceInfo, IrqSourceList, QueueInfo, QueueLimits, Request, RequestFlags, RequestId,
    RequestOp, RequestStatus, validate_request,
};

use crate::{
    Namespace, Nvme,
    err::{Error as NvmeError, Result as NvmeResult},
    queue::{CommandSet, NvmeQueue as HardwareQueue},
};

const MAX_PRP_LIST_PAGES: usize = 1;
const DEFAULT_QUEUE_DEPTH: usize = 64;

struct NvmeBlockInner {
    nvme: Nvme,
    namespace: Namespace,
}

pub struct NvmeBlockDriver {
    name: &'static str,
    inner: Arc<NvmeBlockOwner>,
    queue_depth: usize,
    irq_handler_taken: bool,
}

struct NvmeBlockOwner {
    inner: UnsafeCell<NvmeBlockInner>,
    next_queue_id: AtomicUsize,
    irq_enabled: AtomicBool,
    pending_irq: AtomicU64,
    created_queues: AtomicU64,
}

impl NvmeBlockDriver {
    pub fn from_nvme(mut nvme: Nvme) -> NvmeResult<Self> {
        let namespace = nvme
            .namespace_list()?
            .into_iter()
            .next()
            .ok_or(NvmeError::Unknown("no active namespace found"))?;

        Ok(Self::with_namespace("nvme", nvme, namespace))
    }

    pub fn from_nvme_with_queue_depth(mut nvme: Nvme, queue_depth: usize) -> NvmeResult<Self> {
        let namespace = nvme
            .namespace_list()?
            .into_iter()
            .next()
            .ok_or(NvmeError::Unknown("no active namespace found"))?;

        Ok(Self::with_namespace_and_queue_depth(
            "nvme",
            nvme,
            namespace,
            queue_depth,
        ))
    }

    pub fn with_namespace(name: &'static str, nvme: Nvme, namespace: Namespace) -> Self {
        Self::with_namespace_and_queue_depth(name, nvme, namespace, DEFAULT_QUEUE_DEPTH)
    }

    pub fn with_namespace_and_queue_depth(
        name: &'static str,
        nvme: Nvme,
        namespace: Namespace,
        queue_depth: usize,
    ) -> Self {
        Self {
            name,
            inner: Arc::new(NvmeBlockOwner {
                inner: UnsafeCell::new(NvmeBlockInner { nvme, namespace }),
                next_queue_id: AtomicUsize::new(0),
                irq_enabled: AtomicBool::new(true),
                pending_irq: AtomicU64::new(0),
                created_queues: AtomicU64::new(0),
            }),
            queue_depth: queue_depth.max(1),
            irq_handler_taken: false,
        }
    }

    pub fn namespace(&self) -> Namespace {
        self.inner.with_mut(|inner| inner.namespace)
    }

    pub fn into_interface(self) -> Self {
        self
    }

    fn device_info_for(&self) -> DeviceInfo {
        self.inner
            .with_mut(|inner| device_info(self.name, inner.namespace))
    }

    fn limits_for(&self) -> QueueLimits {
        self.inner.with_mut(|inner| {
            limits(
                inner.nvme.dma_mask(),
                inner.nvme.page_size(),
                inner.namespace,
            )
        })
    }
}

// SAFETY: RDIF queue ownership removes task-side sharing of an IO queue. The
// exported IRQ handler only touches atomics and never borrows the controller.
// The owner keeps the controller and MMIO mapping alive until all queues and
// handlers are dropped.
unsafe impl Send for NvmeBlockOwner {}

// SAFETY: Mutable controller access is scoped through `with_mut` during queue
// creation and namespace queries. Runtime IRQ callbacks use only atomics.
unsafe impl Sync for NvmeBlockOwner {}

impl NvmeBlockOwner {
    fn with_mut<R>(&self, f: impl FnOnce(&mut NvmeBlockInner) -> R) -> R {
        let inner = unsafe { &mut *self.inner.get() };
        f(inner)
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

impl Interface for NvmeBlockDriver {
    fn device_info(&self) -> DeviceInfo {
        self.device_info_for()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.limits_for()
    }

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        let id = self.inner.next_queue_id.fetch_add(1, Ordering::Relaxed);
        if id >= u64::BITS as usize {
            return None;
        }

        let queue = self.inner.with_mut(|inner| {
            let queue = inner.nvme.take_io_queue(id)?;
            let depth = self.queue_depth.min(queue.depth().saturating_sub(1).max(1));
            let prp_lists = alloc_prp_lists(&inner.nvme, depth).ok()?;
            Some(NvmeBlockQueue::new(
                id,
                depth,
                self.name,
                inner.namespace,
                inner.nvme.dma_mask(),
                inner.nvme.page_size(),
                queue,
                prp_lists,
                Arc::clone(&self.inner),
            ))
        })?;

        self.inner
            .created_queues
            .fetch_or(1 << id, Ordering::AcqRel);
        Some(Box::new(queue))
    }

    fn enable_irq(&self) {
        self.inner.irq_enabled.store(true, Ordering::Release);
    }

    fn disable_irq(&self) {
        self.inner.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> IrqSourceList {
        let queues = IdList::from_bits(self.inner.created_queues.load(Ordering::Acquire));
        vec![IrqSourceInfo::legacy(queues)]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        if source_id != 0 || self.irq_handler_taken {
            return None;
        }
        self.irq_handler_taken = true;
        Some(Box::new(NvmeIrqHandler {
            inner: Arc::clone(&self.inner),
        }))
    }
}

struct NvmeIrqHandler {
    inner: Arc<NvmeBlockOwner>,
}

impl IrqHandler for NvmeIrqHandler {
    fn handle_irq(&self) -> Event {
        if !self.inner.irq_enabled.load(Ordering::Acquire) {
            return Event::none();
        }
        let pending = self.inner.pending_irq.swap(0, Ordering::AcqRel);
        if pending == 0 {
            Event::from_queue_bits(self.inner.created_queues.load(Ordering::Acquire))
        } else {
            Event::from_queue_bits(pending)
        }
    }
}

struct NvmeBlockQueue {
    id: usize,
    name: &'static str,
    namespace: Namespace,
    dma_mask: u64,
    page_size: usize,
    queue: HardwareQueue,
    slots: Vec<RequestSlot>,
    free_cids: Vec<usize>,
    free_prp_lists: Vec<CoherentArray<u64>>,
    owner: Arc<NvmeBlockOwner>,
}

struct RequestSlot {
    state: SlotState,
    prp_list: Option<CoherentArray<u64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SlotState {
    Free,
    Pending,
    Complete,
    Failed,
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
        queue: HardwareQueue,
        prp_lists: Vec<CoherentArray<u64>>,
        owner: Arc<NvmeBlockOwner>,
    ) -> Self {
        let mut slots = Vec::with_capacity(depth + 1);
        slots.resize_with(depth + 1, || RequestSlot {
            state: SlotState::Free,
            prp_list: None,
        });
        let free_cids = (1..=depth).rev().collect();

        Self {
            id,
            name,
            namespace,
            dma_mask,
            page_size,
            queue,
            slots,
            free_cids,
            free_prp_lists: prp_lists,
            owner,
        }
    }

    fn queue_info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: device_info(self.name, self.namespace),
            limits: limits(self.dma_mask, self.page_size, self.namespace),
        }
    }

    fn alloc_cid(&mut self) -> Result<usize, BlkError> {
        self.free_cids.pop().ok_or(BlkError::Retry)
    }

    fn free_cid(&mut self, cid: usize) {
        if cid < self.slots.len() {
            if let Some(prp_list) = self.slots[cid].prp_list.take() {
                self.free_prp_lists.push(prp_list);
            }
            self.slots[cid].state = SlotState::Free;
            self.free_cids.push(cid);
        }
    }

    fn build_command(&mut self, cid: usize, request: &Request<'_>) -> Result<CommandSet, BlkError> {
        let cid = u16::try_from(cid).map_err(|_| BlkError::InvalidRequest)?;
        match request.op {
            RequestOp::Read | RequestOp::Write => {
                let prp = self.build_prp_mapping(request)?;
                let command = match request.op {
                    RequestOp::Read => CommandSet::nvm_cmd_read_with_cid(
                        self.namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    RequestOp::Write => CommandSet::nvm_cmd_write_with_cid(
                        self.namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    _ => unreachable!(),
                };
                self.slots[usize::from(cid)].prp_list = prp.prp_list;
                Ok(command)
            }
            RequestOp::Flush => Ok(CommandSet::nvm_cmd_flush_with_cid(self.namespace.id, cid)),
            RequestOp::Discard | RequestOp::WriteZeroes => Err(BlkError::NotSupported),
        }
    }

    fn build_prp_mapping(&mut self, request: &Request<'_>) -> Result<PrpMapping, BlkError> {
        let mut pages = Vec::new();
        for segment in request.segments.iter() {
            push_prp_pages(&mut pages, segment.bus, segment.len, self.page_size)?;
        }
        let prp1 = *pages.first().ok_or(BlkError::InvalidRequest)?;
        let prp2 = match pages.len() {
            1 => 0,
            2 => pages[1],
            _ => {
                let list_entries = self.page_size / core::mem::size_of::<u64>();
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

    fn drain_completions(&mut self) {
        while let Some(completion) = self.queue.poll_completion() {
            let cid = usize::from(completion.command_id);
            if let Some(slot) = self.slots.get_mut(cid) {
                slot.state = if completion.status.is_success() {
                    SlotState::Complete
                } else {
                    SlotState::Failed
                };
            }
        }
    }

    fn insert_pending_irq(&self) {
        if self.owner.irq_enabled.load(Ordering::Acquire) && self.id < u64::BITS as usize {
            self.owner
                .pending_irq
                .fetch_or(1 << self.id, Ordering::AcqRel);
        }
    }
}

// SAFETY: NVMe queues may access submitted request DMA segments until the
// matching completion is reclaimed by `poll_request`. Slots are freed only
// after completion/error, and no segment pointers are accessed after that.
unsafe impl IQueue for NvmeBlockQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        self.queue_info()
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        let info = self.queue_info();
        validate_request(info, &request)?;
        self.drain_completions();

        let cid = self.alloc_cid()?;
        let command = match self.build_command(cid, &request) {
            Ok(command) => command,
            Err(err) => {
                self.free_cid(cid);
                return Err(err);
            }
        };
        self.slots[cid].state = SlotState::Pending;
        self.queue.submit_io_data(command);
        self.insert_pending_irq();
        Ok(RequestId::new(cid))
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        self.drain_completions();

        let cid = usize::from(request);
        match self.slots.get(cid).map(|slot| slot.state) {
            Some(SlotState::Pending) => Ok(RequestStatus::Pending),
            Some(SlotState::Complete) => {
                self.free_cid(cid);
                Ok(RequestStatus::Complete)
            }
            Some(SlotState::Failed) => {
                self.free_cid(cid);
                Err(BlkError::Io)
            }
            Some(SlotState::Free) | None => Err(BlkError::InvalidRequest),
        }
    }
}

fn alloc_prp_lists(nvme: &Nvme, depth: usize) -> NvmeResult<Vec<CoherentArray<u64>>> {
    let mut lists = Vec::with_capacity(depth);
    for _ in 0..depth {
        lists.push(nvme.alloc_prp_list()?);
    }
    Ok(lists)
}

fn push_prp_pages(
    pages: &mut Vec<u64>,
    mut addr: u64,
    mut len: usize,
    page_size: usize,
) -> Result<(), BlkError> {
    if page_size == 0 || len == 0 {
        return Err(BlkError::InvalidRequest);
    }

    while len > 0 {
        pages.push(addr);
        let offset = addr as usize % page_size;
        let chunk = page_size.saturating_sub(offset).min(len);
        if chunk == 0 {
            return Err(BlkError::InvalidRequest);
        }
        addr = addr
            .checked_add(chunk as u64)
            .ok_or(BlkError::InvalidRequest)?;
        len -= chunk;
    }
    Ok(())
}

fn device_info(name: &'static str, namespace: Namespace) -> DeviceInfo {
    DeviceInfo {
        name: Some(name),
        model: Some("nvme"),
        ..DeviceInfo::new(namespace.lba_count as u64, namespace.lba_size)
    }
}

fn limits(dma_mask: u64, page_size: usize, namespace: Namespace) -> QueueLimits {
    let prp_entries = page_size / core::mem::size_of::<u64>();
    let max_bytes = page_size.saturating_mul(prp_entries + 1);
    let max_blocks = max_bytes
        .checked_div(namespace.lba_size.max(1))
        .unwrap_or(1)
        .max(1)
        .min(u16::MAX as usize + 1) as u32;
    QueueLimits {
        dma_mask,
        dma_alignment: namespace.lba_size.max(1),
        max_blocks_per_request: max_blocks,
        max_segments: prp_entries + 1,
        max_segment_size: max_bytes,
        supported_flags: RequestFlags::NONE,
        supports_flush: true,
        supports_discard: false,
        supports_write_zeroes: false,
    }
}
