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
    BlkError, Buffer, IQueue, Interface, QueueConfig, QueueInfo, QueueMode, QueueTopology, Request,
    RequestFlags, RequestId, RequestOp, RequestStatus,
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
    size: usize,
    align: usize,
}

pub struct PlatformBlockDevice {
    name: String,
    interface: Option<Box<dyn Interface>>,
    irq_num: Option<usize>,
}

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
        validate_io(queues.queue.info(), block_id, buf.len())?;

        let block_size = queues.queue.info().device.logical_block_size;
        for (offset, block_buf) in buf.chunks_exact_mut(block_size).enumerate() {
            let mut dma_buffer = queues.pool.alloc(DmaDirection::FromDevice)?;
            dma_buffer.sync_for_device(0, block_size);
            let segment = buffer_from_dma(&mut dma_buffer, block_size);
            let mut segments = [segment];
            let request_id = queues
                .queue
                .submit_request(Request {
                    op: RequestOp::Read,
                    lba: checked_lba(block_id, offset)?,
                    block_count: 1,
                    segments: &mut segments,
                    flags: RequestFlags::NONE,
                })
                .map_err(map_blk_err_to_ax_err)?;
            queues.poll_until_complete(request_id)?;
            dma_buffer.sync_for_cpu(0, block_size);
            dma_buffer.read_with(block_size, |data| block_buf.copy_from_slice(data));
        }
        Ok(())
    }

    pub fn write_block(&mut self, block_id: u64, buf: &[u8]) -> AxResult {
        let mut queues = self.queues.lock();
        validate_io(queues.queue.info(), block_id, buf.len())?;

        let block_size = queues.queue.info().device.logical_block_size;
        for (offset, block_buf) in buf.chunks_exact(block_size).enumerate() {
            let mut dma_buffer = queues.pool.alloc(DmaDirection::ToDevice)?;
            dma_buffer.write_with(block_size, |data| data.copy_from_slice(block_buf));
            dma_buffer.sync_for_device(0, block_size);
            let segment = buffer_from_dma(&mut dma_buffer, block_size);
            let mut segments = [segment];
            let request_id = queues
                .queue
                .submit_request(Request {
                    op: RequestOp::Write,
                    lba: checked_lba(block_id, offset)?,
                    block_count: 1,
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
        let size = info
            .limits
            .max_segment_size
            .min(block_size.max(info.limits.dma_alignment));
        if size < block_size {
            return Err(AxError::BadState);
        }
        let layout = Layout::from_size_align(size, info.limits.dma_alignment.max(1))
            .map_err(|_| AxError::BadState)?;
        Ok(Self {
            queue,
            pool: BlockBufferPool {
                dma: DeviceDma::new(info.limits.dma_mask, axklib::dma::op()),
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
        let topology = interface.queue_topology();
        let queue = interface
            .create_queue(default_queue_config(topology))
            .ok_or(AxError::BadState)?;
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

fn default_queue_config(topology: QueueTopology) -> QueueConfig {
    QueueConfig {
        id_hint: Some(0),
        depth: topology.default_queue_depth.max(1),
        mode: if topology.poll_queue_count > 0 {
            QueueMode::Polled
        } else {
            QueueMode::Interrupt
        },
    }
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

fn checked_lba(start: u64, offset: usize) -> AxResult<u64> {
    start
        .checked_add(offset as u64)
        .ok_or(AxError::InvalidInput)
}

fn buffer_from_dma(buffer: &mut ContiguousArray<u8>, len: usize) -> Buffer<'_> {
    unsafe { Buffer::from_raw_parts(buffer.as_ptr().as_ptr(), buffer.dma_addr().as_u64(), len) }
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
    use rdif_block::{DeviceInfo, DriverGeneric, QueueLimits, validate_request_shape};

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

        fn queue_topology(&self) -> QueueTopology {
            QueueTopology::single(1)
        }

        fn create_queue(&mut self, _config: QueueConfig) -> Option<Box<dyn IQueue>> {
            None
        }
    }

    struct TestQueue {
        dma: &'static TrackingDma,
    }

    impl IQueue for TestQueue {
        fn id(&self) -> usize {
            0
        }

        fn info(&self) -> QueueInfo {
            QueueInfo {
                id: 0,
                depth: 1,
                mode: QueueMode::Polled,
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
            validate_request_shape(self.info().device, self.info().limits, &request)?;
            request.segments[0].fill(0x5a);
            Ok(RequestId::new(0))
        }

        fn poll_request(&mut self, _request: RequestId) -> Result<RequestStatus, BlkError> {
            Ok(RequestStatus::Complete)
        }
    }

    #[test]
    fn read_block_syncs_dma_buffer_for_device_before_submit() {
        let dma = Box::leak(Box::<TrackingDma>::default());
        let mut block = Block {
            name: String::from("test-block"),
            irq_num: None,
            irq_enabled: false,
            #[cfg(feature = "irq")]
            irq_handler: None,
            interface: Box::new(TestInterface),
            queues: SpinNoIrq::new(BlockQueues {
                queue: Box::new(TestQueue { dma }),
                pool: BlockBufferPool {
                    dma: DeviceDma::new(u64::MAX, dma),
                    size: 512,
                    align: 512,
                },
            }),
        };
        let mut buf = [0_u8; 512];

        block.read_block(0, &mut buf).unwrap();

        assert_eq!(buf, [0x5a; 512]);
    }
}
