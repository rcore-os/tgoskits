use alloc::{boxed::Box, vec::Vec};

use rdif_block::{BlkError, Segment, TransferChunk};

/// Direction of a filesystem block transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockDmaDirection {
    Read,
    Write,
}

/// A DMA or bounce buffer whose lifetime covers a submitted block request.
pub trait BlockDmaBuffer: Send {
    fn len(&self) -> usize;
    fn bus_addr(&self) -> u64;
    fn as_mut_ptr(&mut self) -> *mut u8;
    fn prepare_for_submit(&mut self, direction: BlockDmaDirection, src: Option<&[u8]>);
    fn complete_after_submit(&mut self, direction: BlockDmaDirection, dst: Option<&mut [u8]>);

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Runtime-provided DMA capability.
pub trait BlockDmaProvider: Send + Sync {
    fn alloc(
        &self,
        dma_mask: u64,
        len: usize,
        align: usize,
        direction: BlockDmaDirection,
    ) -> Result<Box<dyn BlockDmaBuffer>, BlkError>;
}

/// Guard retained in the pending table until the request reaches completion.
pub struct DmaBufferGuard {
    buffer: Box<dyn BlockDmaBuffer>,
    direction: BlockDmaDirection,
    segments: Box<[Segment<'static>]>,
    len: usize,
}

// SAFETY: `DmaBufferGuard` owns the DMA buffer and every stored segment points
// into that buffer. Moving the guard between threads does not invalidate the
// backing allocation; the request completion contract still requires callers to
// keep the guard alive until `poll_request` reports completion.
unsafe impl Send for DmaBufferGuard {}

impl DmaBufferGuard {
    pub fn new(
        mut buffer: Box<dyn BlockDmaBuffer>,
        direction: BlockDmaDirection,
        chunk: TransferChunk,
        src: Option<&[u8]>,
    ) -> Result<Self, BlkError> {
        let len = buffer.len();
        buffer.prepare_for_submit(direction, src);
        let base_virt = buffer.as_mut_ptr();
        let base_bus = buffer.bus_addr();
        let planned_segments = chunk.segments();
        let mut segments = Vec::with_capacity(planned_segments.len());
        for segment in planned_segments {
            let virt = unsafe { base_virt.add(segment.byte_offset) };
            let bus = base_bus
                .checked_add(segment.byte_offset as u64)
                .ok_or(BlkError::InvalidRequest)?;
            segments.push(unsafe { Segment::from_raw_parts(virt, bus, segment.byte_len) });
        }
        Ok(Self {
            buffer,
            direction,
            segments: segments.into_boxed_slice(),
            len,
        })
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the request segments kept alive by this guard.
    ///
    /// # Safety
    ///
    /// The returned slice may be passed to `submit_request`. The caller must
    /// keep this guard alive and must not access the segments again until the
    /// matching request has completed.
    pub unsafe fn segments_for_submit(&mut self) -> &'static mut [Segment<'static>] {
        unsafe { core::slice::from_raw_parts_mut(self.segments.as_mut_ptr(), self.segments.len()) }
    }

    pub fn complete(mut self, dst: Option<&mut [u8]>) {
        self.buffer.complete_after_submit(self.direction, dst);
    }
}

#[cfg(test)]
pub struct VecDmaBuffer {
    bytes: Vec<u8>,
    bus: u64,
}

#[cfg(test)]
impl VecDmaBuffer {
    pub fn new(len: usize) -> Self {
        let mut bytes = alloc::vec![0; len];
        let bus = bytes.as_mut_ptr() as u64;
        Self { bytes, bus }
    }
}

#[cfg(test)]
impl BlockDmaBuffer for VecDmaBuffer {
    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn bus_addr(&self) -> u64 {
        self.bus
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.bytes.as_mut_ptr()
    }

    fn prepare_for_submit(&mut self, direction: BlockDmaDirection, src: Option<&[u8]>) {
        if direction == BlockDmaDirection::Write
            && let Some(src) = src
        {
            self.bytes[..src.len()].copy_from_slice(src);
        }
    }

    fn complete_after_submit(&mut self, direction: BlockDmaDirection, dst: Option<&mut [u8]>) {
        if direction == BlockDmaDirection::Read
            && let Some(dst) = dst
        {
            dst.copy_from_slice(&self.bytes[..dst.len()]);
        }
    }
}

#[cfg(test)]
pub struct VecDmaProvider;

#[cfg(test)]
impl BlockDmaProvider for VecDmaProvider {
    fn alloc(
        &self,
        _dma_mask: u64,
        len: usize,
        _align: usize,
        _direction: BlockDmaDirection,
    ) -> Result<Box<dyn BlockDmaBuffer>, BlkError> {
        Ok(Box::new(VecDmaBuffer::new(len)))
    }
}
