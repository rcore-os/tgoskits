#![no_std]

extern crate alloc;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_kspin::SpinRaw as Mutex;
use rdif_block::{
    BlkError, DeviceInfo, DriverGeneric, Event, IQueue, IdList, Interface, IrqHandler,
    IrqSourceInfo, IrqSourceList, QueueInfo, QueueLimits, Request, RequestId, RequestOp,
    RequestStatus, validate_request,
};

const PREFERRED_TRANSFER_SIZE: usize = 16 * 1024;

struct RamInner {
    storage: Vec<u8>,
    completed: Vec<RequestId>,
    next_req_id: usize,
    next_queue_id: usize,
}

struct RamIrqState {
    enabled: AtomicBool,
    handler_taken: AtomicBool,
    queues: AtomicU64,
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
            completed: Vec::new(),
            next_req_id: 1,
            next_queue_id: 0,
        };
        let irq = RamIrqState {
            enabled: AtomicBool::new(true),
            handler_taken: AtomicBool::new(false),
            queues: AtomicU64::new(0),
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

    fn device_info_for(&self) -> DeviceInfo {
        DeviceInfo {
            name: Some(self.name),
            ..DeviceInfo::new(self.num_blocks as u64, self.block_size)
        }
    }

    fn limits_for(&self) -> QueueLimits {
        let mut limits = QueueLimits::simple(self.block_size, u64::MAX);
        let transfer_size = align_down(
            PREFERRED_TRANSFER_SIZE.max(self.block_size),
            self.block_size,
        );
        limits.max_blocks_per_request = (transfer_size / self.block_size).max(1) as u32;
        limits.max_segment_size = transfer_size;
        limits
    }
}

fn align_down(value: usize, align: usize) -> usize {
    value / align * align
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
    fn device_info(&self) -> DeviceInfo {
        self.device_info_for()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.limits_for()
    }

    fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
        let mut guard = self.inner.lock();
        let id = guard.next_queue_id;
        guard.next_queue_id += 1;

        if id >= 64 {
            return None;
        }

        Some(Box::new(RamQueue {
            id,
            device: self.device_info_for(),
            limits: self.limits_for(),
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

    fn irq_sources(&self) -> IrqSourceList {
        alloc::vec![IrqSourceInfo::legacy(IdList::from_bits(u64::MAX))]
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        if source_id != 0 {
            return None;
        }
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
    fn handle_irq(&mut self) -> Event {
        if !self.irq.enabled.load(Ordering::Acquire) {
            return Event::none();
        }

        Event::from_queue_bits(self.irq.queues.swap(0, Ordering::AcqRel))
    }
}

struct RamQueue {
    id: usize,
    device: DeviceInfo,
    limits: QueueLimits,
    inner: Arc<Mutex<RamInner>>,
    irq: Arc<RamIrqState>,
}

// SAFETY: ramdisk copies data synchronously during `submit_request`; it stores
// only the completed request ID and never retains request segment pointers.
unsafe impl IQueue for RamQueue {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: self.device,
            limits: self.limits,
        }
    }

    fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
        validate_request(self.info(), &request)?;

        let mut guard = self.inner.lock();
        let req_id = RequestId::new(guard.next_req_id);
        guard.next_req_id += 1;

        match request.op {
            RequestOp::Read => {
                copy_from_storage(&guard.storage, self.device.logical_block_size, &request)?;
            }
            RequestOp::Write => {
                if self.device.read_only {
                    return Err(BlkError::NotSupported);
                }
                copy_to_storage(&mut guard.storage, self.device.logical_block_size, &request)?;
            }
            RequestOp::Flush => {}
            RequestOp::Discard | RequestOp::WriteZeroes => return Err(BlkError::NotSupported),
        }

        guard.completed.push(req_id);
        insert_irq_bit(&self.irq.queues, self.id);
        Ok(req_id)
    }

    fn poll_request(&mut self, request: RequestId) -> Result<RequestStatus, BlkError> {
        let mut guard = self.inner.lock();
        if let Some(pos) = guard.completed.iter().position(|r| *r == request) {
            guard.completed.remove(pos);
            Ok(RequestStatus::Complete)
        } else {
            Ok(RequestStatus::Pending)
        }
    }
}

fn copy_from_storage(
    storage: &[u8],
    block_size: usize,
    request: &Request<'_>,
) -> Result<(), BlkError> {
    let mut offset = request.lba as usize * block_size;
    for segment in request.segments.iter() {
        unsafe {
            core::ptr::copy_nonoverlapping(storage.as_ptr().add(offset), segment.virt, segment.len);
        }
        offset += segment.len;
    }
    Ok(())
}

fn copy_to_storage(
    storage: &mut [u8],
    block_size: usize,
    request: &Request<'_>,
) -> Result<(), BlkError> {
    let mut offset = request.lba as usize * block_size;
    for segment in request.segments.iter() {
        unsafe {
            core::ptr::copy_nonoverlapping(
                segment.virt,
                storage.as_mut_ptr().add(offset),
                segment.len,
            );
        }
        offset += segment.len;
    }
    Ok(())
}

fn insert_irq_bit(bits: &AtomicU64, id: usize) {
    if id < u64::BITS as usize {
        bits.fetch_or(1 << id, Ordering::AcqRel);
    }
}
