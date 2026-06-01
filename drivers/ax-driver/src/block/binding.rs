use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};
use core::alloc::Layout;

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;
use dma_api::{ContiguousArray, DeviceDma, DmaDirection};
use log::{error, warn};
use rdif_block::{
    BlkError, Buffer, IQueue, Interface, QueueInfo, Request, RequestFlags, RequestId, RequestOp,
    RequestStatus, TransferChunk, TransferPlan, TransferPlanner, TransferRuntimeCaps,
};
use rdrive::Device;

pub struct Block {
    name: String,
    irq_num: Option<usize>,
    irq_enabled: bool,
    #[cfg(feature = "irq")]
    irq_handler: Option<BlockIrqHandler>,
    interface: Box<dyn Interface>,
    queues: SpinNoIrq<BlockQueues>,
}

struct BlockQueues {
    queue: Box<dyn IQueue>,
    pool: BlockBufferPool,
}

struct BlockBufferPool {
    dma: DeviceDma,
    planner: TransferPlanner,
    size: usize,
    align: usize,
}

pub struct PlatformBlockDevice {
    name: String,
    interface: Option<Box<dyn Interface>>,
    irq_num: Option<usize>,
}

const MAX_BLOCK_BUFFER_SIZE: usize = 16 * 1024;

impl PlatformBlockDevice {
    fn new(name: String, interface: Box<dyn Interface>, irq_num: Option<usize>) -> Self {
        Self {
            name,
            interface: Some(interface),
            irq_num,
        }
    }
}

impl rdrive::DriverGeneric for PlatformBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

#[cfg(feature = "irq")]
pub struct BlockIrqHandler {
    handler: Box<dyn rdif_block::IrqHandler>,
}

#[cfg(feature = "irq")]
impl BlockIrqHandler {
    fn new(handler: Box<dyn rdif_block::IrqHandler>) -> Self {
        Self { handler }
    }

    pub fn handle(&self) -> rdif_block::Event {
        self.handler.handle_irq()
    }
}

#[cfg(not(feature = "irq"))]
pub struct BlockIrqHandler;

impl Block {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }

    pub fn enable_irq(&mut self) {
        self.interface.enable_irq();
        self.irq_enabled = self.interface.is_irq_enabled();
    }

    pub fn disable_irq(&mut self) {
        self.interface.disable_irq();
        self.irq_enabled = self.interface.is_irq_enabled();
    }

    pub const fn is_irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    #[cfg(feature = "irq")]
    pub fn take_irq_handler(&mut self) -> Option<(usize, BlockIrqHandler)> {
        let irq_num = self.irq_num.take()?;
        let handler = self.irq_handler.take()?;
        Some((irq_num, handler))
    }

    #[cfg(not(feature = "irq"))]
    pub fn take_irq_handler(&mut self) -> Option<(usize, BlockIrqHandler)> {
        let _ = self;
        None
    }

    pub fn num_blocks(&self) -> u64 {
        self.queues.lock().queue.info().device.num_blocks
    }

    pub fn block_size(&self) -> usize {
        self.queues.lock().queue.info().device.logical_block_size
    }

    pub fn flush(&mut self) -> AxResult {
        let mut queues = self.queues.lock();
        let mut segments = [];
        let request = Request {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            segments: &mut segments,
            flags: RequestFlags::NONE,
        };
        let request_id = match queues.queue.submit_request(request) {
            Ok(request_id) => request_id,
            Err(BlkError::NotSupported) => return Ok(()),
            Err(err) => return Err(map_blk_err_to_ax_err(err)),
        };
        queues.poll_until_complete(request_id)
    }

    pub fn read_block(&mut self, block_id: u64, buf: &mut [u8]) -> AxResult {
        let mut queues = self.queues.lock();
        let info = queues.queue.info();
        validate_io(info, block_id, buf.len())?;

        let plan = transfer_plan(queues.pool.planner, block_id, buf.len())?;
        for chunk in plan {
            let block_buf =
                &mut buf[chunk.byte_offset..chunk.byte_offset.saturating_add(chunk.byte_len)];
            let mut dma_buffer = queues.pool.alloc(DmaDirection::FromDevice)?;
            dma_buffer.prepare_for_device(0, block_buf.len());
            let mut segments = segments_from_dma(&mut dma_buffer, chunk)?;
            let request_id = queues
                .queue
                .submit_request(Request {
                    op: RequestOp::Read,
                    lba: chunk.lba,
                    block_count: chunk.block_count,
                    segments: &mut segments,
                    flags: RequestFlags::NONE,
                })
                .map_err(map_blk_err_to_ax_err)?;
            queues.poll_until_complete(request_id)?;
            dma_buffer.copy_from_device_to_slice(block_buf);
        }
        Ok(())
    }

    pub fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let mut queues = self.queues.lock();
        let info = queues.queue.info();
        validate_io(info, block_id, buf.len())?;

        let plan = transfer_plan(queues.pool.planner, block_id, buf.len())?;
        for chunk in plan {
            let block_buf =
                &buf[chunk.byte_offset..chunk.byte_offset.saturating_add(chunk.byte_len)];
            let mut dma_buffer = queues.pool.alloc(DmaDirection::ToDevice)?;
            dma_buffer.copy_to_device_from_slice(block_buf);
            let mut segments = segments_from_dma(&mut dma_buffer, chunk)?;
            let request_id = queues
                .queue
                .submit_request(Request {
                    op: RequestOp::Write,
                    lba: chunk.lba,
                    block_count: chunk.block_count,
                    segments: &mut segments,
                    flags: RequestFlags::NONE,
                })
                .map_err(map_blk_err_to_ax_err)?;
            queues.poll_until_complete(request_id)?;
        }
        Ok(())
    }
}

impl BlockQueues {
    fn new(queue: Box<dyn IQueue>) -> AxResult<Self> {
        let info = queue.info();
        let block_size = info.device.logical_block_size;
        if block_size == 0 {
            return Err(AxError::BadState);
        }
        let planner = block_transfer_planner(info)?;
        let layout = block_buffer_layout(info, planner.chunk_size())?;
        Ok(Self {
            queue,
            pool: BlockBufferPool {
                dma: DeviceDma::new(info.limits.dma_mask, axklib::dma::op()),
                planner,
                size: layout.size(),
                align: layout.align(),
            },
        })
    }

    fn poll_until_complete(&mut self, request: RequestId) -> AxResult {
        loop {
            match self
                .queue
                .poll_request(request)
                .map_err(map_blk_err_to_ax_err)?
            {
                RequestStatus::Complete => return Ok(()),
                RequestStatus::Pending => core::hint::spin_loop(),
            }
        }
    }
}

impl BlockBufferPool {
    fn alloc(&self, direction: DmaDirection) -> AxResult<ContiguousArray<u8>> {
        self.dma
            .contiguous_array_zero_with_align(self.size, self.align, direction)
            .map_err(BlkError::from)
            .map_err(map_blk_err_to_ax_err)
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for Block {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut dev = base.lock().map_err(|_| AxError::BadState)?;
        let name = dev.name.clone();
        let irq_num = dev.irq_num;
        let mut interface = dev.interface.take().ok_or(AxError::BadState)?;
        let queue = interface.create_queue().ok_or(AxError::BadState)?;
        let queues = BlockQueues::new(queue)?;

        #[cfg(feature = "irq")]
        let irq_handler = irq_num
            .and_then(|_| take_legacy_irq_handler(interface.as_mut()))
            .map(BlockIrqHandler::new);
        drop(dev);

        #[cfg(feature = "irq")]
        let irq_num = if irq_handler.is_some() { irq_num } else { None };
        #[cfg(feature = "irq")]
        let irq_handler = irq_handler;
        #[cfg(not(feature = "irq"))]
        let irq_num = {
            let _ = irq_num;
            None
        };

        Ok(Self {
            name,
            irq_num,
            irq_enabled: interface.is_irq_enabled(),
            #[cfg(feature = "irq")]
            irq_handler,
            interface,
            queues: SpinNoIrq::new(queues),
        })
    }
}

pub trait PlatformDeviceBlock {
    fn register_block<T: Interface>(self, dev: T);
    fn register_block_with_irq<T: Interface>(self, dev: T, irq_num: Option<usize>);
}

impl PlatformDeviceBlock for rdrive::PlatformDevice {
    fn register_block<T: Interface>(self, dev: T) {
        self.register_block_with_irq(dev, None);
    }

    fn register_block_with_irq<T: Interface>(self, dev: T, irq_num: Option<usize>) {
        let name = dev.name().to_string();
        self.register(PlatformBlockDevice::new(name, Box::new(dev), irq_num));
    }
}

pub fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    decode_irq_cells(&interrupt.specifier)
}

fn decode_irq_cells(specifier: &[u32]) -> Option<usize> {
    match specifier {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}

pub fn take_block_devices() -> Vec<Block> {
    rdrive::get_list::<PlatformBlockDevice>()
        .into_iter()
        .filter_map(|dev| match Block::try_from(dev) {
            Ok(block) => Some(block),
            Err(err) => {
                warn!("failed to take block device: {err:?}");
                None
            }
        })
        .collect()
}

#[cfg(feature = "irq")]
fn take_legacy_irq_handler(
    interface: &mut dyn Interface,
) -> Option<Box<dyn rdif_block::IrqHandler>> {
    let has_legacy_source = interface.irq_sources().iter().any(|source| source.id == 0);
    has_legacy_source
        .then(|| interface.take_irq_handler(0))
        .flatten()
}

fn validate_io(info: QueueInfo, block_id: u64, len: usize) -> AxResult {
    let block_size = info.device.logical_block_size;
    if block_size == 0 || !len.is_multiple_of(block_size) {
        return Err(AxError::InvalidInput);
    }
    let block_count = len / block_size;
    let end = block_id
        .checked_add(block_count as u64)
        .ok_or(AxError::InvalidInput)?;
    if end > info.device.num_blocks {
        return Err(AxError::InvalidInput);
    }
    Ok(())
}

fn block_buffer_layout(info: QueueInfo, size: usize) -> AxResult<Layout> {
    let block_size = info.device.logical_block_size;
    if size < block_size {
        return Err(AxError::BadState);
    }
    Layout::from_size_align(size, info.limits.dma_alignment.max(1)).map_err(|_| AxError::BadState)
}

fn block_transfer_planner(info: QueueInfo) -> AxResult<TransferPlanner> {
    TransferPlanner::new(
        info.device,
        info.limits,
        TransferRuntimeCaps {
            max_transfer_bytes: MAX_BLOCK_BUFFER_SIZE,
            max_segments: usize::MAX,
        },
    )
    .map_err(map_blk_err_to_ax_err)
}

fn transfer_plan(planner: TransferPlanner, block_id: u64, len: usize) -> AxResult<TransferPlan> {
    planner.plan(block_id, len).map_err(map_blk_err_to_ax_err)
}

fn segments_from_dma(
    buffer: &mut ContiguousArray<u8>,
    chunk: TransferChunk,
) -> AxResult<Vec<Buffer<'_>>> {
    let base_virt = buffer.as_ptr().as_ptr();
    let base_bus = buffer.dma_addr().as_u64();
    let planned_segments = chunk.segments();
    let mut segments = Vec::with_capacity(planned_segments.len());
    for segment in planned_segments {
        let virt = unsafe { base_virt.add(segment.byte_offset) };
        let bus = base_bus
            .checked_add(segment.byte_offset as u64)
            .ok_or(AxError::InvalidInput)?;
        segments.push(unsafe { Buffer::from_raw_parts(virt, bus, segment.byte_len) });
    }
    Ok(segments)
}

fn map_blk_err_to_ax_err(err: BlkError) -> AxError {
    match err {
        BlkError::NotSupported => AxError::Unsupported,
        BlkError::Retry => AxError::WouldBlock,
        BlkError::NoMemory => AxError::NoMemory,
        BlkError::InvalidBlockIndex(_) | BlkError::InvalidRequest => AxError::InvalidInput,
        BlkError::Io => AxError::Io,
        BlkError::Other(error) => {
            error!("Block device error: {error}");
            AxError::Io
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::{boxed::Box, string::String, vec::Vec};
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
    use std::{
        alloc::{alloc_zeroed, dealloc},
        sync::Mutex,
    };

    use dma_api::{
        DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp,
    };
    use rdif_block::{DeviceInfo, DriverGeneric, QueueLimits, validate_request};

    use super::*;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum SyncOp {
        ForDevice {
            size: usize,
            direction: DmaDirection,
        },
        ForCpu {
            size: usize,
            direction: DmaDirection,
        },
    }

    #[derive(Default)]
    struct TrackingDma {
        ops: Mutex<Vec<SyncOp>>,
    }

    impl TrackingDma {
        fn has_read_device_sync(&self) -> bool {
            self.ops.lock().unwrap().iter().any(|op| {
                matches!(
                    op,
                    SyncOp::ForDevice {
                        size: 512,
                        direction: DmaDirection::FromDevice
                    }
                )
            })
        }

        fn sync_count(&self, expected: SyncOp) -> usize {
            self.ops
                .lock()
                .unwrap()
                .iter()
                .filter(|op| **op == expected)
                .count()
        }
    }

    impl DmaOp for TrackingDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let ptr = unsafe { alloc_zeroed(layout) };
            let ptr = NonNull::new(ptr)?;
            Some(unsafe { DmaAllocHandle::new(ptr, (ptr.as_ptr() as u64).into(), layout) })
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            unsafe { self.alloc_contiguous(constraints, layout) }
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
            unsafe { self.dealloc_contiguous(handle) };
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            addr: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            let layout = Layout::from_size_align(size.get(), 1)?;
            Ok(unsafe { DmaMapHandle::new(addr, (addr.as_ptr() as u64).into(), layout, None) })
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}

        fn sync_alloc_for_device(
            &self,
            _handle: &DmaAllocHandle,
            _offset: usize,
            size: usize,
            direction: DmaDirection,
        ) {
            self.ops
                .lock()
                .unwrap()
                .push(SyncOp::ForDevice { size, direction });
        }

        fn sync_alloc_for_cpu(
            &self,
            _handle: &DmaAllocHandle,
            _offset: usize,
            size: usize,
            direction: DmaDirection,
        ) {
            self.ops
                .lock()
                .unwrap()
                .push(SyncOp::ForCpu { size, direction });
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SubmittedRequest {
        op: RequestOp,
        lba: u64,
        block_count: u32,
        data_len: usize,
        segment_lengths: Vec<usize>,
    }

    #[derive(Default)]
    struct RequestLog {
        requests: Mutex<Vec<SubmittedRequest>>,
    }

    impl RequestLog {
        fn push(&self, request: SubmittedRequest) {
            self.requests.lock().unwrap().push(request);
        }

        fn snapshot(&self) -> Vec<SubmittedRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    struct TestInterface;

    impl DriverGeneric for TestInterface {
        fn name(&self) -> &str {
            "test-block"
        }
    }

    impl Interface for TestInterface {
        fn device_info(&self) -> DeviceInfo {
            DeviceInfo {
                name: Some("test-block"),
                ..DeviceInfo::new(8, 512)
            }
        }

        fn queue_limits(&self) -> QueueLimits {
            QueueLimits::simple(512, u64::MAX)
        }

        fn create_queue(&mut self) -> Option<Box<dyn IQueue>> {
            None
        }
    }

    struct TestQueue {
        dma: &'static TrackingDma,
    }

    // SAFETY: The queue copies data synchronously during `submit_request` and
    // never stores segment pointers after the call returns.
    unsafe impl IQueue for TestQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 0,
                device: DeviceInfo {
                    name: Some("test-block"),
                    ..DeviceInfo::new(8, 512)
                },
                limits: QueueLimits::simple(512, u64::MAX),
            }
        }

        fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
            assert!(
                self.dma.has_read_device_sync(),
                "read DMA buffers must be synchronized for device ownership before submit"
            );
            validate_request(self.info(), &request)?;
            request.segments[0].fill(0x5a);
            Ok(RequestId::new(0))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    struct RecordingQueue {
        info: QueueInfo,
        log: &'static RequestLog,
    }

    impl RecordingQueue {
        fn new(log: &'static RequestLog, limits: QueueLimits) -> Self {
            Self {
                info: QueueInfo {
                    id: 0,
                    device: DeviceInfo {
                        name: Some("recording-block"),
                        ..DeviceInfo::new(64, 512)
                    },
                    limits,
                },
                log,
            }
        }
    }

    // SAFETY: The queue records request metadata synchronously and never
    // accesses request segments after `submit_request` returns.
    unsafe impl IQueue for RecordingQueue {
        fn id(&self) -> usize {
            self.info.id
        }

        fn info(&self) -> QueueInfo {
            self.info
        }

        fn submit_request(&mut self, request: Request<'_>) -> Result<RequestId, BlkError> {
            validate_request(self.info, &request)?;
            if request.op == RequestOp::Read {
                request.segments[0].fill(request.block_count as u8);
            }
            self.log.push(SubmittedRequest {
                op: request.op,
                lba: request.lba,
                block_count: request.block_count,
                data_len: request.data_len(),
                segment_lengths: request.segments.iter().map(|segment| segment.len).collect(),
            });
            Ok(RequestId::new(self.log.snapshot().len()))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    fn block_with_queue(queue: Box<dyn IQueue>, dma: &'static TrackingDma) -> Block {
        let info = queue.info();
        let planner = block_transfer_planner(info).unwrap();
        let layout = block_buffer_layout(info, planner.chunk_size()).unwrap();
        Block {
            name: String::from("test-block"),
            irq_num: None,
            irq_enabled: false,
            #[cfg(feature = "irq")]
            irq_handler: None,
            interface: Box::new(TestInterface),
            queues: SpinNoIrq::new(BlockQueues {
                queue,
                pool: BlockBufferPool {
                    dma: DeviceDma::new(u64::MAX, dma),
                    planner,
                    size: layout.size(),
                    align: layout.align(),
                },
            }),
        }
    }

    #[test]
    fn read_block_syncs_dma_buffer_for_device_before_submit() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let mut block = block_with_queue(Box::new(TestQueue { dma }), dma);
        let mut buf = [0_u8; 512];

        block.read_block(0, &mut buf).unwrap();

        assert_eq!(buf, [0x5a; 512]);
    }

    #[test]
    fn write_block_batches_requests_to_queue_limits() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let log = Box::leak(Box::<RequestLog>::default());
        let limits = QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 4096,
            max_blocks_per_request: 8,
            max_segments: 1,
            max_segment_size: 4096,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        };
        let mut block = block_with_queue(Box::new(RecordingQueue::new(log, limits)), dma);
        let buf = [0x42_u8; 8192];

        block.write_block(4, &buf).unwrap();

        assert_eq!(
            log.snapshot(),
            [
                SubmittedRequest {
                    op: RequestOp::Write,
                    lba: 4,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
                SubmittedRequest {
                    op: RequestOp::Write,
                    lba: 12,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
            ]
        );
        assert_eq!(
            dma.sync_count(SyncOp::ForDevice {
                size: 4096,
                direction: DmaDirection::ToDevice,
            }),
            2
        );
    }

    #[test]
    fn read_block_batches_requests_to_queue_limits() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let log = Box::leak(Box::<RequestLog>::default());
        let limits = QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 4096,
            max_blocks_per_request: 8,
            max_segments: 1,
            max_segment_size: 4096,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        };
        let mut block = block_with_queue(Box::new(RecordingQueue::new(log, limits)), dma);
        let mut buf = [0_u8; 8192];

        block.read_block(4, &mut buf).unwrap();

        assert_eq!(
            log.snapshot(),
            [
                SubmittedRequest {
                    op: RequestOp::Read,
                    lba: 4,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
                SubmittedRequest {
                    op: RequestOp::Read,
                    lba: 12,
                    block_count: 8,
                    data_len: 4096,
                    segment_lengths: alloc::vec![4096],
                },
            ]
        );
        assert_eq!(buf, [8; 8192]);
        assert_eq!(
            dma.sync_count(SyncOp::ForDevice {
                size: 4096,
                direction: DmaDirection::FromDevice,
            }),
            2
        );
        assert_eq!(
            dma.sync_count(SyncOp::ForCpu {
                size: 4096,
                direction: DmaDirection::FromDevice,
            }),
            2
        );
    }

    #[test]
    fn block_transfer_planner_caps_large_finite_segments() {
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo {
                name: Some("large-segment-block"),
                ..DeviceInfo::new(64, 512)
            },
            limits: QueueLimits {
                dma_mask: u64::MAX,
                dma_alignment: 4096,
                max_blocks_per_request: 4096,
                max_segments: 1,
                max_segment_size: 2 * 1024 * 1024,
                supported_flags: RequestFlags::NONE,
                supports_flush: false,
                supports_discard: false,
                supports_write_zeroes: false,
            },
        };

        assert_eq!(
            block_transfer_planner(info).unwrap().chunk_size(),
            16 * 1024
        );
    }

    #[test]
    fn block_transfer_planner_uses_simple_limit_preference_for_unbounded_segments() {
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo {
                name: Some("simple-block"),
                ..DeviceInfo::new(64, 512)
            },
            limits: QueueLimits::simple(512, u64::MAX),
        };

        assert_eq!(block_transfer_planner(info).unwrap().chunk_size(), 512);
    }

    #[test]
    fn block_transfer_planner_applies_runtime_cap_without_unbounded_segment_special_case() {
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo {
                name: Some("unbounded-large-preference-block"),
                ..DeviceInfo::new(128, 512)
            },
            limits: QueueLimits {
                dma_mask: u64::MAX,
                dma_alignment: 4096,
                max_blocks_per_request: u32::MAX,
                max_segments: 1,
                max_segment_size: usize::MAX,
                supported_flags: RequestFlags::NONE,
                supports_flush: false,
                supports_discard: false,
                supports_write_zeroes: false,
            },
        };

        assert_eq!(
            block_transfer_planner(info).unwrap().chunk_size(),
            MAX_BLOCK_BUFFER_SIZE
        );
    }

    #[test]
    fn write_block_uses_planned_multi_segment_chunks() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let log = Box::leak(Box::<RequestLog>::default());
        let limits = QueueLimits {
            dma_mask: u64::MAX,
            dma_alignment: 4096,
            max_blocks_per_request: 8,
            max_segments: 4,
            max_segment_size: 1024,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        };
        let mut block = block_with_queue(Box::new(RecordingQueue::new(log, limits)), dma);
        let buf = [0x42_u8; 4096];

        block.write_block(4, &buf).unwrap();

        assert_eq!(
            log.snapshot(),
            [SubmittedRequest {
                op: RequestOp::Write,
                lba: 4,
                block_count: 8,
                data_len: 4096,
                segment_lengths: alloc::vec![1024, 1024, 1024, 1024],
            }]
        );
    }
}
