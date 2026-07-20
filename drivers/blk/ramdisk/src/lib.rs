#![no_std]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};

use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, DeviceInfo, DmaQuiesced, DriverGeneric, IQueue,
    Interface, LifecycleEndpoint, OwnedRequest, QueueEventBatch, QueueExecution, QueueHandle,
    QueueInfo, QueueKind, QueueLimits, RequestId, RequestOp, ServiceProgress, SubmitError,
    SubmitOutcome, validate_owned_request,
};

const PREFERRED_TRANSFER_SIZE: usize = 16 * 1024;

/// CPU-memory-backed block device whose requests always complete inline.
pub struct RamDisk {
    name: &'static str,
    block_size: usize,
    num_blocks: usize,
    storage: Option<Vec<u8>>,
}

impl RamDisk {
    pub fn new(block_size: usize, num_blocks: usize) -> Self {
        Self::with_name("ramdisk", block_size, num_blocks)
    }

    pub fn with_name(name: &'static str, block_size: usize, num_blocks: usize) -> Self {
        assert!(block_size > 0, "block size must be greater than zero");

        let storage_len = block_size
            .checked_mul(num_blocks)
            .expect("ramdisk capacity must fit in usize");
        let mut storage = Vec::with_capacity(storage_len);
        for block in 0..num_blocks {
            storage.extend(core::iter::repeat_n(block as u8, block_size));
        }

        Self {
            name,
            block_size,
            num_blocks,
            storage: Some(storage),
        }
    }

    pub fn block_size(&self) -> usize {
        self.block_size
    }

    pub fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    pub fn storage_len(&self) -> usize {
        // Construction already proved that the product fits in `usize`.
        self.block_size * self.num_blocks
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
        limits.supports_flush = true;
        limits
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
    fn controller_init(&mut self) -> rdif_block::ControllerInitEndpoint<'_> {
        rdif_block::ControllerInitEndpoint::Ready
    }

    fn lifecycle(&mut self) -> LifecycleEndpoint<'_> {
        LifecycleEndpoint::Inline
    }

    fn device_info(&self) -> DeviceInfo {
        self.device_info_for()
    }

    fn queue_limits(&self) -> QueueLimits {
        self.limits_for()
    }

    fn create_queue(&mut self) -> Option<QueueHandle> {
        let storage = self.storage.take()?;

        Some(QueueHandle::new(Box::new(RamQueue {
            info: QueueInfo {
                id: 0,
                device: self.device_info_for(),
                limits: self.limits_for(),
                kind: QueueKind::Inline,
                execution: QueueExecution::Inline,
            },
            storage,
        })))
    }

    fn enable_irq(&self) -> Result<(), BlkError> {
        Err(BlkError::NotSupported)
    }

    fn disable_irq(&self) -> Result<(), BlkError> {
        Err(BlkError::NotSupported)
    }

    fn is_irq_enabled(&self) -> bool {
        false
    }

    fn irq_sources(&self) -> rdif_block::IrqSourceList {
        Vec::new()
    }

    fn take_irq_source(&mut self, _source_id: usize) -> Option<rdif_block::BlockIrqSource> {
        None
    }
}

struct RamQueue {
    info: QueueInfo,
    storage: Vec<u8>,
}

impl IQueue for RamQueue {
    fn id(&self) -> usize {
        self.info.id
    }

    fn info(&self) -> QueueInfo {
        self.info
    }

    fn submit_owned(
        &mut self,
        id: RequestId,
        mut request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        if let Err(error) = validate_owned_request(self.info, &request) {
            return Err(SubmitError::new(id, error, request));
        }

        let result = execute_request(&mut self.storage, self.info.device, &mut request);
        Ok(SubmitOutcome::Completed(CompletedRequest::new(
            id, result, request,
        )))
    }

    fn service_events(
        &mut self,
        _events: &QueueEventBatch<'_>,
        _sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        Err(BlkError::NotSupported)
    }

    fn reclaim_after_quiesce(
        &mut self,
        _proof: &DmaQuiesced,
        _sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        Ok(())
    }
}

fn execute_request(
    storage: &mut [u8],
    device: DeviceInfo,
    request: &mut OwnedRequest,
) -> Result<(), BlkError> {
    let byte_offset = usize::try_from(request.lba)
        .ok()
        .and_then(|lba| lba.checked_mul(device.logical_block_size))
        .ok_or(BlkError::InvalidRequest)?;

    match request.op {
        RequestOp::Read => {
            let data = request.data.as_mut().ok_or(BlkError::InvalidRequest)?;
            let byte_len = data.len().get();
            let source = storage
                .get(byte_offset..byte_offset + byte_len)
                .ok_or(BlkError::InvalidRequest)?;
            // SAFETY: an inline ramdisk never transfers this CPU-owned buffer
            // to hardware, and the queue has exclusive ownership of request.
            unsafe { data.as_mut_slice_cpu() }.copy_from_slice(source);
            Ok(())
        }
        RequestOp::Write => {
            if device.read_only {
                return Err(BlkError::NotSupported);
            }
            let data = request.data.as_ref().ok_or(BlkError::InvalidRequest)?;
            let target = storage
                .get_mut(byte_offset..byte_offset + data.len().get())
                .ok_or(BlkError::InvalidRequest)?;
            target.copy_from_slice(data.as_slice_cpu());
            Ok(())
        }
        RequestOp::Flush => Ok(()),
        RequestOp::Discard | RequestOp::WriteZeroes => Err(BlkError::NotSupported),
    }
}

fn align_down(value: usize, align: usize) -> usize {
    value / align * align
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};
    use std::alloc::{alloc_zeroed, dealloc};

    use rdif_block::{
        IrqSourceList, RequestFlags,
        dma_api::{
            CpuDmaBuffer, DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError,
            DmaMapHandle, DmaOp,
        },
    };

    use super::*;

    struct TestDma;

    impl DmaOp for TestDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
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
    }

    static TEST_DMA: TestDma = TestDma;

    fn dma_buffer(direction: DmaDirection, fill: u8) -> CpuDmaBuffer {
        let dma = DeviceDma::new_legacy(u64::MAX, &TEST_DMA);
        let mut buffer = CpuDmaBuffer::new_zero(
            &dma,
            NonZeroUsize::new(16).expect("test buffer is non-zero"),
            16,
            direction,
        )
        .expect("test DMA allocation must succeed");
        // SAFETY: the freshly allocated buffer has never been device-owned.
        unsafe { buffer.as_mut_slice_cpu() }.fill(fill);
        buffer
    }

    fn request(op: RequestOp, lba: u64, data: Option<CpuDmaBuffer>) -> OwnedRequest {
        OwnedRequest {
            op,
            lba,
            block_count: u32::from(data.is_some()),
            data,
            flags: RequestFlags::NONE,
        }
    }

    fn completed(outcome: SubmitOutcome) -> CompletedRequest {
        match outcome {
            SubmitOutcome::Completed(completion) => completion,
            SubmitOutcome::Queued => panic!("ramdisk must never queue a request"),
        }
    }

    #[test]
    fn queue_declares_inline_execution_without_interrupt_sources() {
        let mut disk = RamDisk::new(16, 8);
        let queue = disk.create_queue().expect("ramdisk queue must be created");
        let sources: IrqSourceList = disk.irq_sources();

        assert_eq!(queue.info().kind, QueueKind::Inline);
        assert_eq!(queue.info().execution, QueueExecution::Inline);
        assert!(sources.is_empty());
        assert!(!disk.is_irq_enabled());
        queue.close().unwrap();
    }

    #[test]
    fn read_and_write_complete_inline_and_return_full_request() {
        let mut disk = RamDisk::new(16, 8);
        let mut queue = disk.create_queue().expect("ramdisk queue must be created");

        let write_id = RequestId::INLINE;
        let write_buffer = dma_buffer(DmaDirection::ToDevice, 0xa5);
        let write_cpu_pointer = write_buffer.cpu_ptr();
        let write_dma_address = write_buffer.dma_addr();
        let write = completed(
            queue
                .submit_owned(write_id, request(RequestOp::Write, 3, Some(write_buffer)))
                .expect("write submission must be accepted"),
        );
        assert_eq!(write.id, write_id);
        assert_eq!(write.result, Ok(()));
        let returned_write_buffer = write
            .request
            .data
            .as_ref()
            .expect("inline completion must return the submitted buffer");
        assert_eq!(returned_write_buffer.cpu_ptr(), write_cpu_pointer);
        assert_eq!(returned_write_buffer.dma_addr(), write_dma_address);

        let read_id = RequestId::INLINE;
        let read = completed(
            queue
                .submit_owned(
                    read_id,
                    request(
                        RequestOp::Read,
                        3,
                        Some(dma_buffer(DmaDirection::FromDevice, 0)),
                    ),
                )
                .expect("read submission must be accepted"),
        );
        assert_eq!(read.id, read_id);
        assert_eq!(read.result, Ok(()));
        assert_eq!(
            read.request
                .data
                .as_ref()
                .expect("read request must retain data")
                .as_slice_cpu(),
            &[0xa5; 16]
        );
        queue.close().unwrap();
    }

    #[test]
    fn rejected_request_returns_runtime_id_and_ownership() {
        let mut disk = RamDisk::new(16, 8);
        let mut queue = disk.create_queue().expect("ramdisk queue must be created");
        let request_id = RequestId::INLINE;

        let error = queue
            .submit_owned(
                request_id,
                request(
                    RequestOp::Read,
                    8,
                    Some(dma_buffer(DmaDirection::FromDevice, 0)),
                ),
            )
            .expect_err("out-of-range request must be rejected");

        assert_eq!(error.id(), request_id);
        assert_eq!(error.error(), BlkError::InvalidBlockIndex(8));
        assert!(error.request().data.is_some());

        let flush = queue
            .submit_owned(RequestId::INLINE, request(RequestOp::Flush, 0, None))
            .expect("a pre-admission rejection must not poison the inline queue");
        assert_eq!(completed(flush).result, Ok(()));
        queue.close().unwrap();
    }

    #[test]
    fn ramdisk_materializes_one_exclusive_inline_queue() {
        let mut disk = RamDisk::new(16, 8);
        let queue = disk
            .create_queue()
            .expect("the ramdisk storage must move into its only queue");

        assert!(
            disk.create_queue().is_none(),
            "an inline ramdisk must not manufacture several locked views of one storage object"
        );
        queue.close().unwrap();
    }
}
