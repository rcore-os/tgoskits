#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use rdif_block::{
    BlkError, CompletedRequest, DeviceInfo, HardwareQueueLimits, InlineBlockDevice,
    InlineBlockDeviceError, InlineExecuteQueue, OwnedRequest, RequestId, RequestOp,
    validate_owned_request_v13,
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

    /// Moves the storage into the call-stack-only rdif-block v0.13 queue.
    ///
    /// The queue can be taken exactly once. It completes every request in
    /// [`InlineExecuteQueue::execute_owned`] and never needs an IRQ, tag, or
    /// completion waiter.
    pub fn take_inline_queue(&mut self) -> Option<RamDiskInlineQueue> {
        self.take_queue_core()
            .map(|core| RamDiskInlineQueue { core })
    }

    /// Consumes the ramdisk into the inline-only registration boundary.
    pub fn into_inline_device(mut self) -> Result<InlineBlockDevice, InlineBlockDeviceError> {
        let device = self.device_info_for();
        let limits = self.limits_for();
        let queue = self
            .take_inline_queue()
            .expect("a newly consumed ramdisk owns exactly one storage vector");
        InlineBlockDevice::new(self.name, device, limits, queue)
    }

    fn device_info_for(&self) -> DeviceInfo {
        DeviceInfo {
            name: Some(self.name),
            ..DeviceInfo::new(self.num_blocks as u64, self.block_size)
        }
    }

    fn limits_for(&self) -> HardwareQueueLimits {
        let mut limits = HardwareQueueLimits::simple(self.block_size, u64::MAX);
        let transfer_size = align_down(
            PREFERRED_TRANSFER_SIZE.max(self.block_size),
            self.block_size,
        );
        limits.max_blocks_per_request = (transfer_size / self.block_size).max(1) as u32;
        limits.max_segment_size = transfer_size;
        limits.supports_flush = true;
        limits
    }

    fn take_queue_core(&mut self) -> Option<RamQueueCore> {
        let device = self.device_info_for();
        let limits = self.limits_for();
        let storage = self.storage.take()?;
        Some(RamQueueCore {
            device,
            limits,
            storage,
        })
    }
}

/// Owned ramdisk queue for the rdif-block v0.13 inline execution boundary.
///
/// This type has no interrupt or asynchronous completion surface: ownership
/// always returns from the same [`InlineExecuteQueue::execute_owned`] call.
#[derive(Debug)]
pub struct RamDiskInlineQueue {
    core: RamQueueCore,
}

impl InlineExecuteQueue for RamDiskInlineQueue {
    fn execute_owned(&mut self, request: OwnedRequest) -> CompletedRequest {
        self.core.complete_inline(request)
    }
}

#[derive(Debug)]
struct RamQueueCore {
    device: DeviceInfo,
    limits: HardwareQueueLimits,
    storage: Vec<u8>,
}

impl RamQueueCore {
    fn complete_inline(&mut self, mut request: OwnedRequest) -> CompletedRequest {
        let result = validate_owned_request_v13(self.device, self.limits, &request)
            .and_then(|()| execute_request(&mut self.storage, self.device, &mut request));
        CompletedRequest::new(RequestId::INLINE, result, request)
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
        RequestFlags,
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

    #[test]
    fn inline_device_publishes_geometry_without_an_irq_contract() {
        let device = RamDisk::new(16, 8)
            .into_inline_device()
            .expect("ramdisk metadata must be valid");

        assert_eq!(device.name(), "ramdisk");
        assert_eq!(device.device_info().num_blocks, 8);
        assert_eq!(device.device_info().logical_block_size, 16);
        assert_eq!(device.device_info().name, Some("ramdisk"));
        assert_eq!(device.limits().max_segments, 1);
    }

    #[test]
    fn read_and_write_complete_inline_and_return_full_request() {
        let mut disk = RamDisk::new(16, 8)
            .into_inline_device()
            .expect("ramdisk metadata must be valid");

        let write_buffer = dma_buffer(DmaDirection::ToDevice, 0xa5);
        let write_cpu_pointer = write_buffer.cpu_ptr();
        let write_dma_address = write_buffer.dma_addr();
        let write = disk.execute_owned(request(RequestOp::Write, 3, Some(write_buffer)));
        assert_eq!(write.id, RequestId::INLINE);
        assert_eq!(write.result, Ok(()));
        let returned_write_buffer = write
            .request
            .data
            .as_ref()
            .expect("inline completion must return the submitted buffer");
        assert_eq!(returned_write_buffer.cpu_ptr(), write_cpu_pointer);
        assert_eq!(returned_write_buffer.dma_addr(), write_dma_address);

        let read = disk.execute_owned(request(
            RequestOp::Read,
            3,
            Some(dma_buffer(DmaDirection::FromDevice, 0)),
        ));
        assert_eq!(read.id, RequestId::INLINE);
        assert_eq!(read.result, Ok(()));
        assert_eq!(
            read.request
                .data
                .as_ref()
                .expect("read request must retain data")
                .as_slice_cpu(),
            &[0xa5; 16]
        );
    }

    #[test]
    fn inline_io_error_returns_the_request_as_a_terminal_completion() {
        let mut disk = RamDisk::new(16, 8)
            .into_inline_device()
            .expect("ramdisk metadata must be valid");
        let completion = disk.execute_owned(request(
            RequestOp::Read,
            8,
            Some(dma_buffer(DmaDirection::FromDevice, 0)),
        ));

        assert_eq!(completion.id, RequestId::INLINE);
        assert_eq!(completion.result, Err(BlkError::InvalidBlockIndex(8)));
        assert!(completion.request.data.is_some());

        let flush = disk.execute_owned(request(RequestOp::Flush, 0, None));
        assert_eq!(flush.result, Ok(()));
    }

    #[test]
    fn ramdisk_materializes_one_exclusive_inline_queue() {
        let mut disk = RamDisk::new(16, 8);
        let _queue = disk
            .take_inline_queue()
            .expect("the ramdisk storage must move into its only queue");

        assert!(
            disk.take_inline_queue().is_none(),
            "an inline ramdisk must not manufacture several locked views of one storage object"
        );
    }
}
