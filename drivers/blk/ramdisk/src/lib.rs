#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use rdif_block::{
    BlkError, BuffConfig, DriverGeneric, Event, IQueue, IdList, Interface, Request, RequestId,
    RequestKind,
};
use spin::Mutex;

struct RamInner {
    storage: Vec<u8>,
    completed_reads: Vec<RequestId>,
    completed_writes: Vec<RequestId>,
    irq_rx: IdList,
    irq_enabled: bool,
    next_req_id: usize,
    next_queue_id: usize,
}

pub struct RamDisk {
    name: &'static str,
    block_size: usize,
    num_blocks: usize,
    inner: Arc<Mutex<RamInner>>,
}

impl RamDisk {
    pub fn new(block_size: usize, num_blocks: usize) -> Self {
        Self::with_name("ramdisk", block_size, num_blocks)
    }

    pub fn with_name(name: &'static str, block_size: usize, num_blocks: usize) -> Self {
        assert!(block_size > 0, "block size must be greater than zero");

        let mut storage = Vec::with_capacity(block_size * num_blocks);
        for i in 0..num_blocks {
            let value = i as u8;
            storage.extend(core::iter::repeat_n(value, block_size));
        }

        let inner = RamInner {
            storage,
            completed_reads: Vec::new(),
            completed_writes: Vec::new(),
            irq_rx: IdList::none(),
            irq_enabled: true,
            next_req_id: 1,
            next_queue_id: 0,
        };

        Self {
            name,
            block_size,
            num_blocks,
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    pub fn storage_len(&self) -> usize {
        self.inner.lock().storage.len()
    }
}

impl DriverGeneric for RamDisk {
    fn name(&self) -> &str {
        self.name
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl Interface for RamDisk {
    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        let mut guard = self.inner.lock();
        let id = guard.next_queue_id;
        guard.next_queue_id += 1;

        Some(Box::new(RamQueue {
            id,
            block_size: self.block_size,
            num_blocks: self.num_blocks,
            inner: Arc::clone(&self.inner),
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
        let mut guard = self.inner.lock();
        if !guard.irq_enabled {
            return Event::none();
        }

        let mut ev = Event::none();
        core::mem::swap(&mut ev.queue, &mut guard.irq_rx);
        ev
    }
}

struct RamQueue {
    id: usize,
    block_size: usize,
    num_blocks: usize,
    inner: Arc<Mutex<RamInner>>,
}

impl IQueue for RamQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn buff_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: !0u64,
            align: 1,
            size: self.block_size,
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        let block_id = request.block_id;
        if block_id >= self.num_blocks {
            return Err(BlkError::InvalidBlockIndex(block_id));
        }

        let mut guard = self.inner.lock();
        let req_id = RequestId::new(guard.next_req_id);
        guard.next_req_id += 1;

        let offset = block_id * self.block_size;
        match request.kind {
            RequestKind::Read(buff) => {
                if buff.size < self.block_size {
                    return Err(BlkError::NotSupported);
                }

                unsafe {
                    core::ptr::copy_nonoverlapping(
                        guard.storage.as_ptr().add(offset),
                        buff.virt,
                        self.block_size,
                    );
                }
                guard.completed_reads.push(req_id);
            }
            RequestKind::Write(slice) => {
                if slice.len() != self.block_size {
                    return Err(BlkError::NotSupported);
                }

                unsafe {
                    core::ptr::copy_nonoverlapping(
                        slice.as_ptr(),
                        guard.storage.as_mut_ptr().add(offset),
                        self.block_size,
                    );
                }
                guard.completed_writes.push(req_id);
            }
        }

        guard.irq_rx.insert(self.id);
        Ok(req_id)
    }

    fn poll_request(&mut self, request: RequestId) -> Result<(), BlkError> {
        let mut guard = self.inner.lock();
        if let Some(pos) = guard.completed_reads.iter().position(|r| *r == request) {
            guard.completed_reads.remove(pos);
            Ok(())
        } else if let Some(pos) = guard.completed_writes.iter().position(|r| *r == request) {
            guard.completed_writes.remove(pos);
            Ok(())
        } else {
            Err(BlkError::Retry)
        }
    }
}
