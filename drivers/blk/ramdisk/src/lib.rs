#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rdif_block::{
    BlkError, BuffConfig, DriverGeneric, Event, IReadQueue, IWriteQueue, Interface, IrqHandler,
    QueueInfo, RequestId, RequestRead, RequestStatus, RequestWrite,
};
use spin::Mutex;

struct RamInner {
    storage: Vec<u8>,
    completed_reads: Vec<RequestId>,
    completed_writes: Vec<RequestId>,
    next_req_id: usize,
    next_read_queue_id: usize,
    next_write_queue_id: usize,
}

struct RamIrqState {
    enabled: AtomicBool,
    handler_taken: AtomicBool,
    read: AtomicU64,
    write: AtomicU64,
}

pub struct RamDisk {
    name: &'static str,
    block_size: usize,
    num_blocks: usize,
    inner: Arc<Mutex<RamInner>>,
    irq: Arc<RamIrqState>,
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
            next_req_id: 1,
            next_read_queue_id: 0,
            next_write_queue_id: 0,
        };
        let irq = RamIrqState {
            enabled: AtomicBool::new(true),
            handler_taken: AtomicBool::new(false),
            read: AtomicU64::new(0),
            write: AtomicU64::new(0),
        };

        Self {
            name,
            block_size,
            num_blocks,
            inner: Arc::new(Mutex::new(inner)),
            irq: Arc::new(irq),
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
    fn create_read_queue(&mut self) -> Option<Box<dyn IReadQueue>> {
        let mut guard = self.inner.lock();
        let id = guard.next_read_queue_id;
        guard.next_read_queue_id += 1;

        Some(Box::new(RamReadQueue {
            id,
            block_size: self.block_size,
            num_blocks: self.num_blocks,
            inner: Arc::clone(&self.inner),
            irq: Arc::clone(&self.irq),
        }))
    }

    fn create_write_queue(&mut self) -> Option<Box<dyn IWriteQueue>> {
        let mut guard = self.inner.lock();
        let id = guard.next_write_queue_id;
        guard.next_write_queue_id += 1;

        Some(Box::new(RamWriteQueue {
            id,
            block_size: self.block_size,
            num_blocks: self.num_blocks,
            inner: Arc::clone(&self.inner),
            irq: Arc::clone(&self.irq),
        }))
    }

    fn enable_irq(&self) {
        self.irq.enabled.store(true, Ordering::Release);
    }

    fn disable_irq(&self) {
        self.irq.enabled.store(false, Ordering::Release);
    }

    fn is_irq_enabled(&self) -> bool {
        self.irq.enabled.load(Ordering::Acquire)
    }

    fn take_irq_handler(&mut self) -> Option<Box<dyn IrqHandler>> {
        self.irq
            .handler_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;

        Some(Box::new(RamIrqHandler {
            irq: Arc::clone(&self.irq),
        }))
    }
}

struct RamIrqHandler {
    irq: Arc<RamIrqState>,
}

impl IrqHandler for RamIrqHandler {
    fn handle_irq(&self) -> Event {
        if !self.irq.enabled.load(Ordering::Acquire) {
            return Event::none();
        }

        let mut ev = Event::none();
        insert_event_bits(&mut ev.read_queue, self.irq.read.swap(0, Ordering::AcqRel));
        insert_event_bits(
            &mut ev.write_queue,
            self.irq.write.swap(0, Ordering::AcqRel),
        );
        ev
    }
}

struct RamReadQueue {
    id: usize,
    block_size: usize,
    num_blocks: usize,
    inner: Arc<Mutex<RamInner>>,
    irq: Arc<RamIrqState>,
}

struct RamWriteQueue {
    id: usize,
    block_size: usize,
    num_blocks: usize,
    inner: Arc<Mutex<RamInner>>,
    irq: Arc<RamIrqState>,
}

impl QueueInfo for RamReadQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn buffer_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: !0u64,
            align: 1,
            size: self.block_size,
        }
    }
}

impl IReadQueue for RamReadQueue {
    fn submit_read(&mut self, request: RequestRead<'_>) -> Result<RequestId, BlkError> {
        let block_id = request.block_id;
        if block_id >= self.num_blocks {
            return Err(BlkError::InvalidBlockIndex(block_id));
        }

        let mut guard = self.inner.lock();
        let req_id = RequestId::new(guard.next_req_id);
        guard.next_req_id += 1;

        let offset = block_id * self.block_size;
        let buff = request.buffer;
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
        insert_irq_bit(&self.irq.read, self.id);
        Ok(req_id)
    }

    fn poll_read(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        let mut guard = self.inner.lock();
        if let Some(pos) = guard.completed_reads.iter().position(|r| *r == request) {
            guard.completed_reads.remove(pos);
            Ok(RequestStatus::Complete)
        } else {
            Ok(RequestStatus::Pending)
        }
    }
}

impl QueueInfo for RamWriteQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    fn block_size(&self) -> usize {
        self.block_size
    }

    fn buffer_config(&self) -> BuffConfig {
        BuffConfig {
            dma_mask: !0u64,
            align: 1,
            size: self.block_size,
        }
    }
}

impl IWriteQueue for RamWriteQueue {
    fn submit_write(&mut self, request: RequestWrite<'_>) -> Result<RequestId, BlkError> {
        let block_id = request.block_id;
        if block_id >= self.num_blocks {
            return Err(BlkError::InvalidBlockIndex(block_id));
        }

        let mut guard = self.inner.lock();
        let req_id = RequestId::new(guard.next_req_id);
        guard.next_req_id += 1;

        let offset = block_id * self.block_size;
        let buff = request.buffer;
        if buff.size < self.block_size {
            return Err(BlkError::NotSupported);
        }

        unsafe {
            core::ptr::copy_nonoverlapping(
                buff.virt,
                guard.storage.as_mut_ptr().add(offset),
                self.block_size,
            );
        }
        guard.completed_writes.push(req_id);
        insert_irq_bit(&self.irq.write, self.id);
        Ok(req_id)
    }

    fn poll_write(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        let mut guard = self.inner.lock();
        if let Some(pos) = guard.completed_writes.iter().position(|r| *r == request) {
            guard.completed_writes.remove(pos);
            Ok(RequestStatus::Complete)
        } else {
            Ok(RequestStatus::Pending)
        }
    }
}

fn insert_irq_bit(bits: &AtomicU64, id: usize) {
    if id < u64::BITS as usize {
        bits.fetch_or(1 << id, Ordering::AcqRel);
    }
}

fn insert_event_bits(list: &mut rdif_block::IdList, bits: u64) {
    for id in 0..u64::BITS as usize {
        if bits & (1 << id) != 0 {
            list.insert(id);
        }
    }
}
