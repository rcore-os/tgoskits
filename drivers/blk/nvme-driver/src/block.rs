use alloc::{boxed::Box, collections::BTreeSet, sync::Arc};
use core::{
    any::Any,
    cell::UnsafeCell,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use rdif_block::{
    BlkError, BuffConfig, DriverGeneric, Event, IReadQueue, IWriteQueue, Interface, IrqHandler,
    QueueInfo, RequestId, RequestRead, RequestStatus, RequestWrite,
};

use crate::{Namespace, Nvme, err::Result as NvmeResult};

struct NvmeBlockInner {
    nvme: Nvme,
    namespace: Namespace,
    completed_reads: BTreeSet<RequestId>,
    completed_writes: BTreeSet<RequestId>,
}

pub struct NvmeBlockDriver {
    name: &'static str,
    inner: Arc<NvmeBlockOwner>,
    irq_handler_taken: bool,
}

struct NvmeBlockOwner {
    inner: UnsafeCell<NvmeBlockInner>,
    next_request_id: AtomicUsize,
    next_read_queue_id: AtomicUsize,
    next_write_queue_id: AtomicUsize,
    irq_enabled: AtomicBool,
    pending_read_irq: AtomicU64,
    pending_write_irq: AtomicU64,
}

impl NvmeBlockDriver {
    pub fn from_nvme(mut nvme: Nvme) -> NvmeResult<Self> {
        let namespace = nvme
            .namespace_list()?
            .into_iter()
            .next()
            .ok_or(crate::err::Error::Unknown("no active namespace found"))?;

        Ok(Self::with_namespace("nvme", nvme, namespace))
    }

    pub fn with_namespace(name: &'static str, nvme: Nvme, namespace: Namespace) -> Self {
        Self {
            name,
            inner: Arc::new(NvmeBlockOwner {
                inner: UnsafeCell::new(NvmeBlockInner {
                    nvme,
                    namespace,
                    completed_reads: BTreeSet::new(),
                    completed_writes: BTreeSet::new(),
                }),
                next_request_id: AtomicUsize::new(1),
                next_read_queue_id: AtomicUsize::new(0),
                next_write_queue_id: AtomicUsize::new(0),
                irq_enabled: AtomicBool::new(true),
                pending_read_irq: AtomicU64::new(0),
                pending_write_irq: AtomicU64::new(0),
            }),
            irq_handler_taken: false,
        }
    }

    pub fn namespace(&self) -> Namespace {
        self.inner.with_mut(|inner| inner.namespace)
    }

    pub fn into_interface(self) -> Self {
        self
    }
}

// SAFETY: The rdif block integration serializes task-side queue access, and the
// IRQ handler only touches atomic event bits. The `Nvme` value, including its
// `mmio-api::Mmio` owner, remains alive inside this shared owner.
unsafe impl Send for NvmeBlockOwner {}

// SAFETY: See `Send`. Mutable access to non-atomic fields is scoped through
// `with_mut`; IRQ event extraction does not borrow those fields.
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
    fn create_read_queue(&mut self) -> Option<Box<dyn IReadQueue>> {
        let id = self
            .inner
            .next_read_queue_id
            .fetch_add(1, Ordering::Relaxed);
        Some(Box::new(NvmeReadQueue {
            id,
            inner: self.inner.clone(),
        }))
    }

    fn create_write_queue(&mut self) -> Option<Box<dyn IWriteQueue>> {
        let id = self
            .inner
            .next_write_queue_id
            .fetch_add(1, Ordering::Relaxed);
        Some(Box::new(NvmeWriteQueue {
            id,
            inner: self.inner.clone(),
        }))
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

    fn take_irq_handler(&mut self) -> Option<Box<dyn IrqHandler>> {
        if self.irq_handler_taken {
            return None;
        }
        self.irq_handler_taken = true;
        Some(Box::new(NvmeIrqHandler {
            inner: self.inner.clone(),
        }))
    }
}

struct NvmeIrqHandler {
    inner: Arc<NvmeBlockOwner>,
}

impl IrqHandler for NvmeIrqHandler {
    fn handle_irq(&self) -> Event {
        let mut event = Event::none();
        if !self.inner.irq_enabled.load(Ordering::Acquire) {
            return event;
        }
        let read = self.inner.pending_read_irq.swap(0, Ordering::AcqRel);
        let write = self.inner.pending_write_irq.swap(0, Ordering::AcqRel);
        for id in 0..64 {
            if read & (1 << id) != 0 {
                event.read_queue.insert(id);
            }
            if write & (1 << id) != 0 {
                event.write_queue.insert(id);
            }
        }
        event
    }
}

struct NvmeReadQueue {
    id: usize,
    inner: Arc<NvmeBlockOwner>,
}

struct NvmeWriteQueue {
    id: usize,
    inner: Arc<NvmeBlockOwner>,
}

impl QueueInfo for NvmeReadQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.with_mut(|inner| inner.namespace.lba_count)
    }

    fn block_size(&self) -> usize {
        self.inner.with_mut(|inner| inner.namespace.lba_size)
    }

    fn buffer_config(&self) -> BuffConfig {
        self.inner.with_mut(|inner| BuffConfig {
            dma_mask: inner.nvme.dma_mask(),
            align: inner.namespace.lba_size,
            size: inner.namespace.lba_size,
        })
    }
}

impl IReadQueue for NvmeReadQueue {
    fn submit_read(
        &mut self,
        request: RequestRead<'_>,
    ) -> core::result::Result<RequestId, BlkError> {
        self.inner.with_mut(|inner| {
            let namespace = inner.namespace;

            if request.block_id >= namespace.lba_count {
                return Err(BlkError::InvalidBlockIndex(request.block_id));
            }

            let req_id = RequestId::new(self.inner.next_request_id.fetch_add(1, Ordering::Relaxed));

            let buffer = request.buffer;
            if buffer.size < namespace.lba_size {
                return Err(BlkError::NotSupported);
            }

            let slice = unsafe { core::slice::from_raw_parts_mut(buffer.virt, namespace.lba_size) };
            inner
                .nvme
                .block_read_sync(&namespace, request.block_id as u64, slice)
                .map_err(|err| BlkError::Other(Box::new(err)))?;

            inner.completed_reads.insert(req_id);
            if self.inner.irq_enabled.load(Ordering::Acquire) {
                self.inner
                    .pending_read_irq
                    .fetch_or(1 << self.id, Ordering::AcqRel);
            }

            Ok(req_id)
        })
    }

    fn poll_read(&mut self, request: RequestId) -> core::result::Result<RequestStatus, BlkError> {
        self.inner.with_mut(|inner| {
            if inner.completed_reads.remove(&request) {
                Ok(RequestStatus::Complete)
            } else {
                Ok(RequestStatus::Pending)
            }
        })
    }
}

impl QueueInfo for NvmeWriteQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.with_mut(|inner| inner.namespace.lba_count)
    }

    fn block_size(&self) -> usize {
        self.inner.with_mut(|inner| inner.namespace.lba_size)
    }

    fn buffer_config(&self) -> BuffConfig {
        self.inner.with_mut(|inner| BuffConfig {
            dma_mask: inner.nvme.dma_mask(),
            align: inner.namespace.lba_size,
            size: inner.namespace.lba_size,
        })
    }
}

impl IWriteQueue for NvmeWriteQueue {
    fn submit_write(
        &mut self,
        request: RequestWrite<'_>,
    ) -> core::result::Result<RequestId, BlkError> {
        self.inner.with_mut(|inner| {
            let namespace = inner.namespace;

            if request.block_id >= namespace.lba_count {
                return Err(BlkError::InvalidBlockIndex(request.block_id));
            }

            let req_id = RequestId::new(self.inner.next_request_id.fetch_add(1, Ordering::Relaxed));

            let buffer = request.buffer;
            if buffer.len() != namespace.lba_size {
                return Err(BlkError::NotSupported);
            }

            inner
                .nvme
                .block_write_sync(&namespace, request.block_id as u64, &buffer)
                .map_err(|err| BlkError::Other(Box::new(err)))?;

            inner.completed_writes.insert(req_id);
            if self.inner.irq_enabled.load(Ordering::Acquire) {
                self.inner
                    .pending_write_irq
                    .fetch_or(1 << self.id, Ordering::AcqRel);
            }

            Ok(req_id)
        })
    }

    fn poll_write(&mut self, request: RequestId) -> core::result::Result<RequestStatus, BlkError> {
        self.inner.with_mut(|inner| {
            if inner.completed_writes.remove(&request) {
                Ok(RequestStatus::Complete)
            } else {
                Ok(RequestStatus::Pending)
            }
        })
    }
}
