use core::any::Any;

use alloc::{boxed::Box, collections::BTreeSet, sync::Arc};
use rd_block::{
    BlkError, Block as RdBlock, BuffConfig, DriverGeneric, Event, IQueue, IdList, Interface,
    Request, RequestId, RequestKind,
};
use spin::Mutex;

use crate::{Namespace, Nvme, err::Result};

struct NvmeBlockInner {
    nvme: Nvme,
    namespace: Namespace,
    irq_enabled: bool,
    pending_irq: IdList,
    completed: BTreeSet<RequestId>,
    next_request_id: usize,
    next_queue_id: usize,
}

pub struct NvmeBlockDriver {
    name: &'static str,
    inner: Arc<Mutex<NvmeBlockInner>>,
}

impl NvmeBlockDriver {
    pub fn from_nvme(mut nvme: Nvme) -> Result<Self> {
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
            inner: Arc::new(Mutex::new(NvmeBlockInner {
                nvme,
                namespace,
                irq_enabled: true,
                pending_irq: IdList::none(),
                completed: BTreeSet::new(),
                next_request_id: 1,
                next_queue_id: 0,
            })),
        }
    }

    pub fn namespace(&self) -> Namespace {
        self.inner.lock().namespace
    }

    pub fn into_block(self, dma_op: &'static dyn dma_api::DmaOp) -> RdBlock {
        RdBlock::new(self, dma_op)
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
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        let mut inner = self.inner.lock();
        let queue_id = inner.next_queue_id;
        inner.next_queue_id += 1;

        Some(Box::new(NvmeRequestQueue {
            id: queue_id,
            inner: self.inner.clone(),
        }))
    }

    fn enable_irq(&mut self) {
        self.inner.lock().irq_enabled = true;
    }

    fn disable_irq(&mut self) {
        self.inner.lock().irq_enabled = false;
    }

    fn is_irq_enabled(&self) -> bool {
        self.inner.lock().irq_enabled
    }

    fn handle_irq(&mut self) -> Event {
        let mut inner = self.inner.lock();
        if !inner.irq_enabled {
            return Event::none();
        }

        let mut event = Event::none();
        core::mem::swap(&mut event.queue, &mut inner.pending_irq);
        event
    }
}

struct NvmeRequestQueue {
    id: usize,
    inner: Arc<Mutex<NvmeBlockInner>>,
}

impl IQueue for NvmeRequestQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.inner.lock().namespace.lba_count
    }

    fn block_size(&self) -> usize {
        self.inner.lock().namespace.lba_size
    }

    fn buff_config(&self) -> BuffConfig {
        let inner = self.inner.lock();
        BuffConfig {
            dma_mask: inner.nvme.dma_mask(),
            align: inner.namespace.lba_size,
            size: inner.namespace.lba_size,
        }
    }

    fn submit_request(
        &mut self,
        request: Request<'_>,
    ) -> core::result::Result<RequestId, BlkError> {
        let mut inner = self.inner.lock();
        let namespace = inner.namespace;

        if request.block_id >= namespace.lba_count {
            return Err(BlkError::InvalidBlockIndex(request.block_id));
        }

        let req_id = RequestId::new(inner.next_request_id);
        inner.next_request_id += 1;

        match request.kind {
            RequestKind::Read(buffer) => {
                if buffer.size < namespace.lba_size {
                    return Err(BlkError::NotSupported);
                }

                let slice =
                    unsafe { core::slice::from_raw_parts_mut(buffer.virt, namespace.lba_size) };
                inner
                    .nvme
                    .block_read_sync(&namespace, request.block_id as u64, slice)
                    .map_err(|err| BlkError::Other(Box::new(err)))?;
            }
            RequestKind::Write(buffer) => {
                if buffer.len() != namespace.lba_size {
                    return Err(BlkError::NotSupported);
                }

                inner
                    .nvme
                    .block_write_sync(&namespace, request.block_id as u64, buffer)
                    .map_err(|err| BlkError::Other(Box::new(err)))?;
            }
        }

        inner.completed.insert(req_id);
        if inner.irq_enabled {
            inner.pending_irq.insert(self.id);
        }

        Ok(req_id)
    }

    fn poll_request(&mut self, request: RequestId) -> core::result::Result<(), BlkError> {
        let mut inner = self.inner.lock();
        if inner.completed.remove(&request) {
            Ok(())
        } else {
            Err(BlkError::Retry)
        }
    }
}
