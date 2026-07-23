use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
use core::{
    any::Any,
    cell::UnsafeCell,
    hint::spin_loop,
    sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicUsize, Ordering},
};

use dma_api::CoherentArray;
use log::warn;
use rdif_block::{
    BlkError, CompletionSink, DeviceInfo, DriverGeneric, Event, IQueue, IdList, Interface,
    IrqHandler, IrqSourceInfo, IrqSourceList, QueueInfo, QueueLimits, Request, RequestFlags,
    RequestId, RequestOp, RequestStatus, validate_request,
};

use crate::{
    Namespace, Nvme,
    err::{Error as NvmeError, Result as NvmeResult},
    queue::{CommandSet, NvmeCompletion, NvmeQueue as HardwareQueue},
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
}

struct NvmeBlockOwner {
    inner: UnsafeCell<NvmeBlockInner>,
    queues: UnsafeCell<Vec<Arc<NvmeQueueCore>>>,
    next_queue_id: AtomicUsize,
    created_queue_bits: AtomicU64,
    irq_enabled: AtomicBool,
    irq_handler_taken_bits: AtomicU64,
    irq_supported: bool,
    msix_interrupts: bool,
    interrupt_vectors: Vec<u16>,
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
        let irq_supported = nvme.io_queue_interrupts_enabled();
        let msix_interrupts = nvme.msix_interrupts_enabled();
        let interrupt_vectors = nvme.interrupt_vectors().to_vec();
        Self {
            name,
            inner: Arc::new(NvmeBlockOwner {
                inner: UnsafeCell::new(NvmeBlockInner { nvme, namespace }),
                queues: UnsafeCell::new(Vec::new()),
                next_queue_id: AtomicUsize::new(0),
                created_queue_bits: AtomicU64::new(0),
                irq_enabled: AtomicBool::new(false),
                irq_handler_taken_bits: AtomicU64::new(0),
                irq_supported,
                msix_interrupts,
                interrupt_vectors,
            }),
            queue_depth: queue_depth.max(1),
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
                inner.nvme.max_transfer_bytes(),
                inner.namespace,
                self.queue_depth,
            )
        })
    }
}

// SAFETY: RDIF queue ownership removes task-side sharing of an IO queue. IRQ
// sharing is mediated by per-queue `NvmeQueueCore` claim guards, and the owner
// keeps the controller and MMIO mapping alive until all queues are dropped.
unsafe impl Send for NvmeBlockOwner {}

// SAFETY: Mutable controller access is scoped through `with_mut` during queue
// creation and namespace queries. The queue registry is populated before the
// IRQ handler is taken and then read-only for the handler lifetime.
unsafe impl Sync for NvmeBlockOwner {}

impl NvmeBlockOwner {
    fn with_mut<R>(&self, f: impl FnOnce(&mut NvmeBlockInner) -> R) -> R {
        let inner = unsafe { &mut *self.inner.get() };
        f(inner)
    }

    fn register_queue(&self, queue: Arc<NvmeQueueCore>) {
        let queues = unsafe { &mut *self.queues.get() };
        queues.push(queue);
    }

    fn queues(&self) -> &[Arc<NvmeQueueCore>] {
        unsafe { &*self.queues.get() }
    }

    fn source_queue_bits(&self, source_id: usize, queue_bits: u64) -> u64 {
        source_queue_bits(
            self.msix_interrupts,
            &self.interrupt_vectors,
            source_id,
            queue_bits,
        )
    }

    fn irq_sources_from_queue_bits(&self, queue_bits: u64) -> IrqSourceList {
        irq_sources_from_queue_bits(self.msix_interrupts, &self.interrupt_vectors, queue_bits)
    }

    fn unique_interrupt_vectors(&self) -> Vec<u16> {
        unique_interrupt_vectors(&self.interrupt_vectors)
    }
}

fn vector_for_queue(msix_interrupts: bool, vectors: &[u16], queue_id: usize) -> Option<u16> {
    if msix_interrupts {
        vectors.get(queue_id).copied()
    } else {
        Some(0)
    }
}

fn source_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    source_id: usize,
    queue_bits: u64,
) -> u64 {
    if !msix_interrupts {
        return if source_id == 0 { queue_bits } else { 0 };
    }

    let mut bits = 0;
    for queue_id in 0..u64::BITS as usize {
        if queue_bits & (1 << queue_id) == 0 {
            continue;
        }
        if vector_for_queue(msix_interrupts, vectors, queue_id) == Some(source_id as u16) {
            bits |= 1 << queue_id;
        }
    }
    bits
}

fn irq_sources_from_queue_bits(
    msix_interrupts: bool,
    vectors: &[u16],
    queue_bits: u64,
) -> IrqSourceList {
    if !msix_interrupts {
        return vec![IrqSourceInfo::legacy(IdList::from_bits(queue_bits))];
    }

    let mut sources = Vec::new();
    for vector in unique_interrupt_vectors(vectors) {
        let queues = source_queue_bits(msix_interrupts, vectors, usize::from(vector), queue_bits);
        if queues != 0 {
            sources.push(IrqSourceInfo::new(
                usize::from(vector),
                IdList::from_bits(queues),
            ));
        }
    }
    sources
}

fn unique_interrupt_vectors(vectors: &[u16]) -> Vec<u16> {
    let mut unique = Vec::new();
    for vector in vectors {
        if !unique.contains(vector) {
            unique.push(*vector);
        }
    }
    unique
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
            Some(NvmeQueueCore::new(
                id,
                depth,
                self.name,
                inner.namespace,
                inner.nvme.dma_mask(),
                inner.nvme.page_size(),
                inner.nvme.max_transfer_bytes(),
                queue,
                prp_lists,
            ))
        })?;

        self.inner.register_queue(queue.clone());
        self.inner
            .created_queue_bits
            .fetch_or(1 << id, Ordering::Release);
        Some(Box::new(NvmeBlockQueue { core: queue }))
    }

    fn enable_irq(&self) {
        if !self.inner.irq_supported {
            return;
        }
        self.inner.with_mut(|inner| {
            for vector in self.inner.unique_interrupt_vectors() {
                inner.nvme.unmask_interrupt_vector(u32::from(vector));
            }
        });
        self.inner.irq_enabled.store(true, Ordering::Release);
    }

    fn disable_irq(&self) {
        if !self.inner.irq_supported {
            return;
        }
        self.inner.with_mut(|inner| {
            for vector in self.inner.unique_interrupt_vectors() {
                inner.nvme.mask_interrupt_vector(u32::from(vector));
            }
        });
        self.inner.irq_enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.irq_supported && self.inner.irq_enabled.load(Ordering::Acquire)
    }

    fn irq_sources(&self) -> IrqSourceList {
        let queue_bits = self.inner.created_queue_bits.load(Ordering::Acquire);
        if !self.inner.irq_supported || queue_bits == 0 {
            return Vec::new();
        }
        self.inner.irq_sources_from_queue_bits(queue_bits)
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        if !self.inner.irq_supported || source_id >= u64::BITS as usize {
            return None;
        }
        let queue_bits = self.inner.source_queue_bits(
            source_id,
            self.inner.created_queue_bits.load(Ordering::Acquire),
        );
        if queue_bits == 0 {
            return None;
        }
        let bit = 1_u64 << source_id;
        if self
            .inner
            .irq_handler_taken_bits
            .fetch_or(bit, Ordering::AcqRel)
            & bit
            != 0
        {
            return None;
        }
        Some(Box::new(NvmeBlockIrqHandler {
            owner: self.inner.clone(),
            source_id,
        }))
    }
}

struct NvmeBlockIrqHandler {
    owner: Arc<NvmeBlockOwner>,
    source_id: usize,
}

impl IrqHandler for NvmeBlockIrqHandler {
    fn handle_irq(&mut self) -> Event {
        if !self.owner.irq_enabled.load(Ordering::Acquire) {
            return Event::none();
        }
        let mut event = Event::none();
        let source_queue_bits = self.owner.source_queue_bits(
            self.source_id,
            self.owner.created_queue_bits.load(Ordering::Acquire),
        );
        for queue in self.owner.queues() {
            if source_queue_bits & (1 << queue.id()) == 0 {
                continue;
            }
            if queue.drain_irq_completions() {
                event.push_queue(queue.id());
            }
        }
        event
    }
}

struct NvmeBlockQueue {
    core: Arc<NvmeQueueCore>,
}

struct NvmeQueueCore {
    id: usize,
    name: &'static str,
    namespace: Namespace,
    dma_mask: u64,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    depth: usize,
    queue: UnsafeCell<HardwareQueue>,
    state: UnsafeCell<NvmeQueueState>,
    completion_cache: CompletionCache,
    state_claimed: AtomicBool,
    cq_claimed: AtomicBool,
}

struct NvmeQueueState {
    slots: Vec<RequestSlot>,
    free_cids: Vec<usize>,
    free_prp_lists: Vec<CoherentArray<u64>>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CachedCompletion {
    cid: usize,
    status: CompletionStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompletionStatus {
    success: bool,
    raw_status: u16,
    result: u64,
}

struct CompletionCache {
    entries: Vec<CompletionCacheEntry>,
}

struct CompletionCacheEntry {
    ready: AtomicBool,
    success: AtomicBool,
    raw_status: AtomicU16,
    result: AtomicU64,
}

struct PrpMapping {
    prp1: u64,
    prp2: u64,
    prp_list: Option<CoherentArray<u64>>,
}

impl NvmeQueueCore {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: usize,
        depth: usize,
        name: &'static str,
        namespace: Namespace,
        dma_mask: u64,
        page_size: usize,
        max_transfer_bytes: Option<usize>,
        queue: HardwareQueue,
        prp_lists: Vec<CoherentArray<u64>>,
    ) -> Arc<Self> {
        let mut slots = Vec::with_capacity(depth + 1);
        slots.resize_with(depth + 1, || RequestSlot {
            state: SlotState::Free,
            prp_list: None,
        });
        let free_cids = (1..=depth).rev().collect();

        Arc::new(Self {
            id,
            name,
            namespace,
            dma_mask,
            page_size,
            max_transfer_bytes,
            depth,
            queue: UnsafeCell::new(queue),
            state: UnsafeCell::new(NvmeQueueState {
                slots,
                free_cids,
                free_prp_lists: prp_lists,
            }),
            completion_cache: CompletionCache::new(depth + 1),
            state_claimed: AtomicBool::new(false),
            cq_claimed: AtomicBool::new(false),
        })
    }

    const fn id(&self) -> usize {
        self.id
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

    fn with_claim<R>(&self, f: impl FnOnce(&mut NvmeQueueState) -> R) -> R {
        while self
            .state_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        let state = unsafe { &mut *self.state.get() };
        let result = f(state);
        self.state_claimed.store(false, Ordering::Release);
        result
    }

    fn with_cq_claim<R>(&self, f: impl FnOnce(&HardwareQueue) -> R) -> R {
        while self
            .cq_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        let queue = unsafe { &*self.queue.get() };
        let result = f(queue);
        self.cq_claimed.store(false, Ordering::Release);
        result
    }

    fn try_with_cq_claim<R>(&self, f: impl FnOnce(&HardwareQueue) -> R) -> Option<R> {
        if self
            .cq_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        let queue = unsafe { &*self.queue.get() };
        let result = f(queue);
        self.cq_claimed.store(false, Ordering::Release);
        Some(result)
    }

    fn drain_irq_completions(&self) -> bool {
        self.try_with_cq_claim(|queue| {
            drain_hardware_completions_to_cache(queue, &self.completion_cache)
        })
        .unwrap_or(true)
    }

    fn drain_completions(&self) -> bool {
        self.with_cq_claim(|queue| {
            drain_hardware_completions_to_cache(queue, &self.completion_cache)
        })
    }
}

// SAFETY: Slot, CID, and completion-cache access is serialized through
// `state_claimed`; hardware CQ access is serialized through `cq_claimed`. SQ
// submission is only driven by the single RDIF queue owner. The MMIO mapping
// and DMA buffers outlive this core through the owner/interface lifetime.
unsafe impl Send for NvmeQueueCore {}

// SAFETY: Shared references are used by task context and hard IRQ context, but
// shared mutable state is guarded as described in the `Send` impl.
unsafe impl Sync for NvmeQueueCore {}

impl NvmeQueueState {
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

    fn build_command(
        &mut self,
        namespace: Namespace,
        page_size: usize,
        cid: usize,
        request: &Request<'_>,
    ) -> Result<CommandSet, BlkError> {
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
                self.slots[usize::from(cid)].prp_list = prp.prp_list;
                Ok(command)
            }
            RequestOp::Flush => Ok(CommandSet::nvm_cmd_flush_with_cid(namespace.id, cid)),
            RequestOp::Discard | RequestOp::WriteZeroes => Err(BlkError::NotSupported),
        }
    }

    fn build_prp_mapping(
        &mut self,
        page_size: usize,
        request: &Request<'_>,
    ) -> Result<PrpMapping, BlkError> {
        let mut prps = PrpPageAccumulator::new();
        for segment in request.segments.iter() {
            prps.push_segment(segment.bus, segment.len, page_size)?;
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

    fn consume_cached_completions(&mut self, queue_id: usize, cache: &CompletionCache) -> usize {
        cache.drain_into_slots(queue_id, &mut self.slots)
    }
}

fn drain_hardware_completions_to_cache(queue: &HardwareQueue, cache: &CompletionCache) -> bool {
    let mut completed = false;
    while let Some(completion) = queue.poll_completion() {
        cache.record(CachedCompletion::from(completion));
        completed = true;
    }
    completed
}

impl CompletionCache {
    fn new(capacity: usize) -> Self {
        let mut entries = Vec::with_capacity(capacity);
        entries.resize_with(capacity, CompletionCacheEntry::new);
        Self { entries }
    }

    fn record(&self, completion: CachedCompletion) {
        let Some(entry) = self.entries.get(completion.cid) else {
            return;
        };
        entry
            .success
            .store(completion.status.success, Ordering::Relaxed);
        entry
            .raw_status
            .store(completion.status.raw_status, Ordering::Relaxed);
        entry
            .result
            .store(completion.status.result, Ordering::Relaxed);
        entry.ready.store(true, Ordering::Release);
    }

    fn drain_into_slots(&self, queue_id: usize, slots: &mut [RequestSlot]) -> usize {
        let mut consumed = 0;
        for (cid, entry) in self.entries.iter().enumerate() {
            if !entry.ready.swap(false, Ordering::AcqRel) {
                continue;
            }
            let Some(slot) = slots.get_mut(cid) else {
                continue;
            };
            let status = CompletionStatus {
                success: entry.success.load(Ordering::Relaxed),
                raw_status: entry.raw_status.load(Ordering::Relaxed),
                result: entry.result.load(Ordering::Relaxed),
            };
            slot.state = if status.success {
                SlotState::Complete
            } else {
                warn!(
                    "nvme queue {} request {} failed: status={:#x}, result={:#x}",
                    queue_id, cid, status.raw_status, status.result
                );
                SlotState::Failed
            };
            consumed += 1;
        }
        consumed
    }
}

impl CompletionCacheEntry {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            success: AtomicBool::new(false),
            raw_status: AtomicU16::new(0),
            result: AtomicU64::new(0),
        }
    }
}

impl From<NvmeCompletion> for CachedCompletion {
    fn from(completion: NvmeCompletion) -> Self {
        Self {
            cid: usize::from(completion.command_id),
            status: CompletionStatus {
                success: completion.status.is_success(),
                raw_status: completion.status.0,
                result: completion.result,
            },
        }
    }
}

// SAFETY: NVMe queues may access submitted request DMA segments until the
// matching completion is reclaimed by `poll_request`. Slots are freed only
// after completion/error, and no segment pointers are accessed after that.
unsafe impl IQueue for NvmeBlockQueue {
    fn id(&self) -> usize {
        self.core.id()
    }

    fn info(&self) -> QueueInfo {
        self.core.queue_info()
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        let info = self.core.queue_info();
        validate_request(info, &request)?;
        let namespace = self.core.namespace;
        let page_size = self.core.page_size;
        let queue_id = self.core.id();

        self.core.drain_completions();
        self.core.with_claim(|state| {
            state.consume_cached_completions(queue_id, &self.core.completion_cache);

            let cid = state.alloc_cid()?;
            let command = match state.build_command(namespace, page_size, cid, &request) {
                Ok(command) => command,
                Err(err) => {
                    state.free_cid(cid);
                    return Err(err);
                }
            };
            state.slots[cid].state = SlotState::Pending;
            let queue = unsafe { &*self.core.queue.get() };
            queue.submit_io_data(command);
            Ok(RequestId::new(cid))
        })
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        let queue_id = self.core.id();
        self.core.drain_completions();
        self.core.with_claim(|state| {
            state.consume_cached_completions(queue_id, &self.core.completion_cache);

            let cid = usize::from(request);
            match state.slots.get(cid).map(|slot| slot.state) {
                Some(SlotState::Pending) => Ok(RequestStatus::Pending),
                Some(SlotState::Complete) => {
                    state.free_cid(cid);
                    Ok(RequestStatus::Complete)
                }
                Some(SlotState::Failed) => {
                    state.free_cid(cid);
                    Err(BlkError::Io)
                }
                Some(SlotState::Free) | None => Err(BlkError::InvalidRequest),
            }
        })
    }

    fn poll_completions(
        &mut self,
        requests: &[RequestId],
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        let queue_id = self.core.id();
        self.core.drain_completions();
        self.core.with_claim(|state| {
            state.consume_cached_completions(queue_id, &self.core.completion_cache);

            for &request in requests {
                let cid = usize::from(request);
                match state.slots.get(cid).map(|slot| slot.state) {
                    Some(SlotState::Pending) => {}
                    Some(SlotState::Complete) => {
                        state.free_cid(cid);
                        sink.complete(request, Ok(()));
                    }
                    Some(SlotState::Failed) => {
                        state.free_cid(cid);
                        sink.complete(request, Err(BlkError::Io));
                    }
                    Some(SlotState::Free) | None => {
                        sink.complete(request, Err(BlkError::InvalidRequest))
                    }
                }
            }
            Ok(())
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
    let dma_alignment = page_size.max(lba_size);
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
        dma_domain: dma_api::DmaDomainId::identity(),
        dma_alignment,
        max_inflight: max_inflight.max(1),
        max_blocks_per_request: max_blocks,
        max_segments: prp_entries + 1,
        max_segment_size: max_bytes,
        supported_flags: RequestFlags::NONE,
        // Do not advertise flush until the driver plumbs a reliable capability
        // check from Identify/Feature data. Some QEMU NVMe backends reject the
        // Flush command with "Invalid Field", which must not surface as fsync
        // I/O errors.
        supports_flush: false,
        supports_discard: false,
        supports_write_zeroes: false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CachedCompletion, CompletionCache, CompletionStatus, PrpPageAccumulator, RequestSlot,
        SlotState, irq_sources_from_queue_bits, limits, source_queue_bits,
    };
    use crate::Namespace;

    #[test]
    fn queue_limits_align_dma_to_nvme_page_size() {
        let namespace = Namespace {
            id: 1,
            lba_size: 512,
            lba_count: 1024,
            metadata_size: 0,
        };
        let limits = limits(u64::MAX, 4096, None, namespace, 8);

        assert_eq!(limits.dma_alignment, 4096);
        assert_eq!(limits.max_segments, 513);
        assert_eq!(limits.max_segment_size, 4096 * 513);
        assert!(limits.max_blocks_per_request >= 8);
        assert!(!limits.supports_flush);
    }

    #[test]
    fn queue_limits_keep_prp_capacity_tied_to_controller_page() {
        let namespace = Namespace {
            id: 1,
            lba_size: 8192,
            lba_count: 1024,
            metadata_size: 0,
        };
        let limits = limits(u64::MAX, 4096, None, namespace, 8);

        assert_eq!(limits.dma_alignment, 8192);
        assert_eq!(limits.max_segments, 513);
        assert_eq!(limits.max_segment_size, 8192 * 256);
        assert_eq!(limits.max_blocks_per_request, 256);
    }

    #[test]
    fn queue_limits_respect_controller_transfer_limit() {
        let namespace = Namespace {
            id: 1,
            lba_size: 512,
            lba_count: 1024,
            metadata_size: 0,
        };
        let limits = limits(u64::MAX, 4096, Some(512 * 1024), namespace, 8);

        assert_eq!(limits.max_blocks_per_request, 1024);
        assert_eq!(limits.max_segment_size, 512 * 1024);
    }

    #[test]
    fn legacy_irq_source_covers_all_created_queues() {
        let sources = irq_sources_from_queue_bits(false, &[], 0b1011);

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, 0);
        assert_eq!(sources[0].queues.bits(), 0b1011);
        assert_eq!(source_queue_bits(false, &[], 0, 0b1011), 0b1011);
        assert_eq!(source_queue_bits(false, &[], 1, 0b1011), 0);
    }

    #[test]
    fn msix_irq_sources_group_queues_by_vector() {
        let vectors = [4, 5, 4];
        let sources = irq_sources_from_queue_bits(true, &vectors, 0b111);

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].id, 4);
        assert_eq!(sources[0].queues.bits(), 0b101);
        assert_eq!(sources[1].id, 5);
        assert_eq!(sources[1].queues.bits(), 0b010);
        assert_eq!(source_queue_bits(true, &vectors, 4, 0b111), 0b101);
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

    #[test]
    fn cached_completion_does_not_complete_slot_until_task_consumes_it() {
        let cache = CompletionCache::new(4);
        let mut slots = test_slots(4);
        slots[2].state = SlotState::Pending;

        cache.record(CachedCompletion::success(2));

        assert_eq!(slots[2].state, SlotState::Pending);
        assert_eq!(cache.drain_into_slots(0, &mut slots), 1);
        assert_eq!(slots[2].state, SlotState::Complete);
    }

    #[test]
    fn cached_failed_completion_marks_slot_failed_in_task_context() {
        let cache = CompletionCache::new(4);
        let mut slots = test_slots(4);
        slots[3].state = SlotState::Pending;

        cache.record(CachedCompletion::failed(3, 0x4002));

        assert_eq!(cache.drain_into_slots(0, &mut slots), 1);
        assert_eq!(slots[3].state, SlotState::Failed);
    }

    #[test]
    fn cached_completion_is_consumed_once() {
        let cache = CompletionCache::new(2);
        let mut slots = test_slots(2);
        slots[1].state = SlotState::Pending;

        cache.record(CachedCompletion::success(1));

        assert_eq!(cache.drain_into_slots(0, &mut slots), 1);
        assert_eq!(cache.drain_into_slots(0, &mut slots), 0);
        assert_eq!(slots[1].state, SlotState::Complete);
    }

    fn test_slots(count: usize) -> alloc::vec::Vec<RequestSlot> {
        (0..count)
            .map(|_| RequestSlot {
                state: SlotState::Free,
                prp_list: None,
            })
            .collect()
    }

    impl CachedCompletion {
        const fn success(cid: usize) -> Self {
            Self {
                cid,
                status: CompletionStatus {
                    success: true,
                    raw_status: 0,
                    result: 0,
                },
            }
        }

        const fn failed(cid: usize, raw_status: u16) -> Self {
            Self {
                cid,
                status: CompletionStatus {
                    success: false,
                    raw_status,
                    result: 0,
                },
            }
        }
    }
}
