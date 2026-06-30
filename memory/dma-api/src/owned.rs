use core::{mem::ManuallyDrop, num::NonZeroUsize, ptr::NonNull};

use crate::{ContiguousArray, DeviceDma, DmaAddr, DmaDirection, DmaDomainId, DmaError};

/// One device-visible DMA segment owned by a prepared request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DmaSegment {
    pub addr: DmaAddr,
    pub len: NonZeroUsize,
}

impl DmaSegment {
    pub const fn new(addr: DmaAddr, len: NonZeroUsize) -> Self {
        Self { addr, len }
    }
}

/// CPU-owned contiguous DMA buffer that can be prepared for one async request.
pub struct CpuDmaBuffer {
    backing: ContiguousArray<u8>,
    direction: DmaDirection,
    domain: DmaDomainId,
}

impl CpuDmaBuffer {
    pub fn new_zero(
        device: &DeviceDma,
        len: NonZeroUsize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let backing =
            device.contiguous_array_zero_with_align(len.get(), align.max(1), direction)?;
        Ok(Self::from_contiguous(backing))
    }

    pub fn from_contiguous(backing: ContiguousArray<u8>) -> Self {
        assert!(
            !backing.is_empty(),
            "CpuDmaBuffer backing must be non-empty"
        );
        let direction = backing.direction();
        let domain = backing.domain_id();
        Self {
            backing,
            direction,
            domain,
        }
    }

    pub fn len(&self) -> NonZeroUsize {
        NonZeroUsize::new(self.backing.bytes_len())
            .expect("CpuDmaBuffer never owns zero-sized backing")
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    pub const fn direction(&self) -> DmaDirection {
        self.direction
    }

    pub const fn domain_id(&self) -> DmaDomainId {
        self.domain
    }

    pub fn cpu_ptr(&self) -> NonNull<u8> {
        self.backing.as_ptr()
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.backing.dma_addr()
    }

    pub fn segment(&self) -> DmaSegment {
        DmaSegment::new(self.dma_addr(), self.len())
    }

    pub fn as_slice_cpu(&self) -> &[u8] {
        self.backing.as_slice_cpu()
    }

    /// # Safety
    ///
    /// The caller must ensure no device can access this buffer while the
    /// returned mutable CPU slice is used.
    pub unsafe fn as_mut_slice_cpu(&mut self) -> &mut [u8] {
        unsafe { self.backing.as_mut_slice_cpu() }
    }

    pub fn copy_to_device_from_slice(&mut self, src: &[u8]) {
        self.backing.copy_to_device_from_slice(src);
    }

    pub fn copy_from_device_to_slice(&self, dst: &mut [u8]) {
        self.backing.copy_from_device_to_slice(dst);
    }

    pub fn prepare_for_device_all(&self) {
        self.backing.prepare_for_device_all();
    }

    pub fn complete_for_cpu_all(&self) {
        self.backing.complete_for_cpu_all();
    }

    pub fn prepare_for_device(self) -> PreparedDma {
        self.prepare_for_device_all();
        PreparedDma { buffer: self }
    }
}

/// DMA backing prepared for device access but not yet owned by hardware.
pub struct PreparedDma {
    buffer: CpuDmaBuffer,
}

impl PreparedDma {
    pub fn len(&self) -> NonZeroUsize {
        self.buffer.len()
    }

    pub const fn direction(&self) -> DmaDirection {
        self.buffer.direction()
    }

    pub const fn domain_id(&self) -> DmaDomainId {
        self.buffer.domain_id()
    }

    pub fn cpu_ptr(&self) -> NonNull<u8> {
        self.buffer.cpu_ptr()
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.buffer.dma_addr()
    }

    pub fn segment(&self) -> DmaSegment {
        self.buffer.segment()
    }

    pub fn segments(&self) -> [DmaSegment; 1] {
        [self.segment()]
    }

    pub fn into_cpu_buffer(self) -> CpuDmaBuffer {
        self.buffer
    }

    /// # Safety
    ///
    /// The caller must start hardware ownership using this prepared backing
    /// and later return it only after hardware is quiesced.
    pub unsafe fn into_in_flight(self) -> InFlightDma {
        InFlightDma {
            prepared: ManuallyDrop::new(self),
        }
    }
}

/// DMA backing currently owned by a hardware request.
///
/// Dropping this object intentionally leaks the backing as a last-resort
/// quarantine: safe callers must not observe memory reuse while hardware could
/// still be accessing it.
pub struct InFlightDma {
    prepared: ManuallyDrop<PreparedDma>,
}

impl InFlightDma {
    pub fn len(&self) -> NonZeroUsize {
        self.prepared.len()
    }

    pub fn direction(&self) -> DmaDirection {
        self.prepared.direction()
    }

    pub fn domain_id(&self) -> DmaDomainId {
        self.prepared.domain_id()
    }

    pub fn cpu_ptr(&self) -> NonNull<u8> {
        self.prepared.cpu_ptr()
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.prepared.dma_addr()
    }

    pub fn segment(&self) -> DmaSegment {
        self.prepared.segment()
    }

    /// # Safety
    ///
    /// The caller must have stopped DMA bus-master access and any command/data
    /// engine that can touch this exact in-flight backing.
    pub unsafe fn complete_after_quiesce(mut self) -> CompletedDma {
        let prepared = unsafe { ManuallyDrop::take(&mut self.prepared) };
        if matches!(
            prepared.buffer.direction(),
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ) {
            prepared.buffer.complete_for_cpu_all();
        }
        CompletedDma {
            buffer: prepared.buffer,
        }
    }

    pub fn quarantine(mut self) -> QuarantinedDma {
        let prepared = unsafe { ManuallyDrop::take(&mut self.prepared) };
        QuarantinedDma {
            prepared: ManuallyDrop::new(prepared),
        }
    }
}

/// DMA backing completed by hardware and visible to CPU again.
pub struct CompletedDma {
    buffer: CpuDmaBuffer,
}

impl CompletedDma {
    pub fn len(&self) -> NonZeroUsize {
        self.buffer.len()
    }

    pub const fn direction(&self) -> DmaDirection {
        self.buffer.direction()
    }

    pub fn copy_from_device_to_slice(&self, dst: &mut [u8]) {
        self.buffer.copy_from_device_to_slice(dst);
    }

    pub fn into_cpu_buffer(self) -> CpuDmaBuffer {
        self.buffer
    }
}

/// DMA backing that cannot yet be safely recycled.
///
/// This type deliberately has no accessor to recover the CPU buffer. Dropping
/// it leaks the backing, preserving the safety invariant.
pub struct QuarantinedDma {
    prepared: ManuallyDrop<PreparedDma>,
}

impl QuarantinedDma {
    pub fn len(&self) -> NonZeroUsize {
        self.prepared.len()
    }

    pub fn domain_id(&self) -> DmaDomainId {
        self.prepared.domain_id()
    }
}
