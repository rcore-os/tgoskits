use core::{num::NonZeroUsize, ops::Range};

use ax_memory_addr::{PhysAddrRange, VirtAddr};
use ax_task::sync::Mutex;
use dma_api::{ContiguousArray, DeviceDma, DmaDirection, DmaError};
use rdif_gpu::{Backing, DmaDomainId, DmaSegment, GpuError};

/// DMA-visible framebuffer memory retained by the GPU resource.
///
/// CPU access is serialized by the inner mutex. The GPU control owner holds
/// its device mutex while calling `with_cpu_bytes`, so a transfer command
/// cannot race with this CPU borrow. Scanout uses the host copy of a 2D
/// resource and only reads guest memory during an explicit transfer.
pub(crate) struct DmaFramebuffer {
    bytes: Mutex<ContiguousArray<u8>>,
    segments: [DmaSegment; 1],
    len: usize,
    domain: DmaDomainId,
}

impl DmaFramebuffer {
    pub(crate) fn allocate(dma: &DeviceDma, len: NonZeroUsize) -> Result<Self, GpuError> {
        let page_size = dma.page_size();
        let alloc_len = len
            .get()
            .checked_add(page_size - 1)
            .map(|size| size / page_size * page_size)
            .ok_or(GpuError::InvalidArgument)?;
        let alloc_len = NonZeroUsize::new(alloc_len).ok_or(GpuError::InvalidArgument)?;
        let bytes = dma
            .contiguous_array_zero_with_align(
                alloc_len.get(),
                page_size,
                DmaDirection::Bidirectional,
            )
            .map_err(|error| match error {
                DmaError::NoMemory => GpuError::OutOfMemory,
                DmaError::LayoutError(_) | DmaError::ZeroSizedBuffer => GpuError::InvalidArgument,
                _ => GpuError::NotAvailable,
            })?;
        let segments = [DmaSegment::new(bytes.dma_addr(), alloc_len)];
        Ok(Self {
            domain: bytes.domain_id(),
            bytes: Mutex::new(bytes),
            segments,
            len: alloc_len.get(),
        })
    }

    fn valid_range(&self, range: &Range<usize>) -> bool {
        range.start <= range.end && range.end <= self.len
    }

    pub(crate) fn physical_range(&self) -> PhysAddrRange {
        let start = self.bytes.lock().as_ptr().as_ptr() as usize;
        let phys = ax_hal::mem::virt_to_phys(VirtAddr::from(start));
        PhysAddrRange::from_start_size(phys, self.len)
    }
}

// SAFETY: ContiguousArray owns a stable DMA mapping for its full lifetime;
// its single segment covers `len`. The mutex serializes all CPU mutable
// borrows. VirtIO GPU 2D transfers only read guest memory on explicit device
// commands, which the ax-gpu owner serializes against framebuffer callbacks.
unsafe impl Backing for DmaFramebuffer {
    fn len(&self) -> usize {
        self.len
    }

    fn domain_id(&self) -> DmaDomainId {
        self.domain
    }

    fn segments(&self) -> &[DmaSegment] {
        &self.segments
    }

    fn sync_for_device(&self, range: Range<usize>) -> Result<(), GpuError> {
        if !self.valid_range(&range) {
            return Err(GpuError::InvalidArgument);
        }
        self.bytes.lock().prepare_for_device(range);
        Ok(())
    }

    fn sync_for_cpu(&self, range: Range<usize>) -> Result<(), GpuError> {
        if !self.valid_range(&range) {
            return Err(GpuError::InvalidArgument);
        }
        self.bytes.lock().complete_for_cpu(range);
        Ok(())
    }

    unsafe fn with_cpu_bytes(&self, access: &mut dyn FnMut(&mut [u8])) -> Result<(), GpuError> {
        let mut bytes = self.bytes.lock();
        bytes.complete_for_cpu(0..self.len);
        // SAFETY: the caller of this unsafe method excludes CPU aliases and
        // has completed all device writes. The inner mutex excludes another
        // callback, and the GPU owner excludes a new transfer during it.
        bytes.write_with_cpu(self.len, access);
        bytes.prepare_for_device(0..self.len);
        Ok(())
    }
}

impl DmaFramebuffer {
    /// Copies from DMA memory without manufacturing a Rust reference to it.
    /// The caller serializes GPU commands through the outer runtime lock.
    pub(crate) fn read_at(&self, output: &mut [u8], offset: usize) -> usize {
        if output.is_empty() || offset >= self.len {
            return 0;
        }
        let count = output.len().min(self.len - offset);
        let bytes = self.bytes.lock();
        bytes.complete_for_cpu(offset..offset + count);
        let ptr = bytes.as_ptr().as_ptr();
        for (index, slot) in output[..count].iter_mut().enumerate() {
            // SAFETY: the DMA allocation remains mapped while `bytes` is held;
            // the checked range is within its length. Volatile access does not
            // create a Rust reference that aliases a userspace mmap.
            *slot = unsafe { core::ptr::read_volatile(ptr.add(offset + index)) };
        }
        count
    }

    /// Copies into DMA memory without manufacturing a mutable Rust slice.
    /// The caller serializes GPU commands through the outer runtime lock.
    pub(crate) fn write_at(&self, input: &[u8], offset: usize) -> Result<usize, GpuError> {
        if input.is_empty() {
            return Ok(0);
        }
        if offset >= self.len {
            return Err(GpuError::InvalidArgument);
        }
        let count = input.len().min(self.len - offset);
        let bytes = self.bytes.lock();
        bytes.complete_for_cpu(offset..offset + count);
        let ptr = bytes.as_ptr().as_ptr();
        for (index, value) in input[..count].iter().copied().enumerate() {
            // SAFETY: the stable DMA allocation covers the checked range.
            // The CPU lock serializes kernel writers; volatile access avoids
            // an exclusive Rust reference to concurrently mapped user pages.
            unsafe { core::ptr::write_volatile(ptr.add(offset + index), value) };
        }
        bytes.prepare_for_device(offset..offset + count);
        Ok(count)
    }
}
