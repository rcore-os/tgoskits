use core::{num::NonZeroUsize, ptr::NonNull};

use mbarrier::mb;

use crate::{
    DmaAllocHandle, DmaCoherency, DmaConstraints, DmaDirection, DmaDomainId, DmaError, DmaMapHandle,
};

cfg_if::cfg_if! {
    if #[cfg(target_arch = "aarch64")] {
        #[path = "aarch64.rs"]
        pub mod arch;
    } else{
        #[path = "nop.rs"]
        pub mod arch;
    }
}

pub trait DmaOp: Sync + Send + 'static {
    fn page_size(&self) -> usize;

    /// Identity of the device address space implemented by this backend.
    ///
    /// Translated backends must override this method. It is checked when a
    /// device capability is constructed, so metadata alone cannot claim an
    /// IOMMU domain while its backend still returns physical addresses.
    fn domain_id(&self) -> DmaDomainId {
        DmaDomainId::Direct
    }

    /// Allocates a device-visible contiguous DMA address range.
    ///
    /// The returned CPU mapping is normal memory. Non-coherent platforms must
    /// use `sync_alloc_for_device` and `sync_alloc_for_cpu` to transfer
    /// ownership between CPU and device.
    ///
    /// # Safety
    ///
    /// Implementations must return a live allocation described by `layout`,
    /// with a DMA address range satisfying `constraints`, and that allocation
    /// must remain valid until `dealloc_contiguous`.
    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle>;

    /// Fallible allocation for backends that distinguish physical memory,
    /// IOVA exhaustion, and page-table update failure.
    ///
    /// A translated backend must override this method and only return a
    /// handle after all translations are visible to the device. Partial
    /// mapping failure must unwind completed mappings and synchronize the
    /// IOTLB before reusing the pages or IOVA.
    ///
    /// # Safety
    ///
    /// The returned allocation obeys the same lifetime and layout contract
    /// as `alloc_contiguous`.
    unsafe fn try_alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        unsafe { self.alloc_contiguous(constraints, layout) }.ok_or(DmaError::NoMemory)
    }

    /// # Safety
    ///
    /// Must be paired with `alloc_contiguous`.
    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle);

    /// Releases an allocation only after device translations are invalidated.
    ///
    /// If invalidation fails, the backend must retain the physical storage
    /// and IOVA in quarantine because the device may still access them. The
    /// handle is consumed regardless of the result.
    ///
    /// # Safety
    ///
    /// Must be paired with `try_alloc_contiguous` or `alloc_contiguous`;
    /// the device must no longer own the buffer.
    unsafe fn try_dealloc_contiguous(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        unsafe { self.dealloc_contiguous(handle) };
        Ok(())
    }

    /// Creates a coherent CPU mapping for a non-coherent DMA device.
    ///
    /// `DeviceDma` calls this branch only when the device is non-coherent.
    /// Coherent devices retain the normal mapping returned by
    /// `alloc_contiguous`. Ordering barriers remain the driver's responsibility.
    ///
    /// # Safety
    ///
    /// Implementations must return a live allocation described by `layout`,
    /// with a DMA address range satisfying `constraints`, and with the backend's
    /// coherent mapping policy applied until `dealloc_coherent`. When the CPU
    /// mapping is an alias, the handle must retain the original allocation
    /// address privately for release.
    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle>;

    /// Coherent allocation with typed failure reasons.
    ///
    /// # Safety
    ///
    /// The returned allocation obeys the same lifetime and layout contract
    /// as `alloc_coherent`.
    unsafe fn try_alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        unsafe { self.alloc_coherent(constraints, layout) }.ok_or(DmaError::NoMemory)
    }

    /// # Safety
    ///
    /// Must be paired with `alloc_coherent`. The handle is consumed even when
    /// this operation fails. On failure the implementation must quarantine the
    /// allocation instead of returning its storage to the allocator.
    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError>;

    /// Maps an existing caller-owned buffer for streaming DMA.
    ///
    /// # Safety
    ///
    /// `addr..addr + size` must remain live until `unmap_streaming`, and CPU
    /// access while the device owns the mapping must follow the sync contract.
    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError>;

    /// # Safety
    ///
    /// Must be paired with `map_streaming`.
    unsafe fn unmap_streaming(&self, handle: DmaMapHandle);

    /// Unmaps a streaming buffer and synchronizes device TLB invalidation.
    ///
    /// On failure the backend must quarantine any physical bounce storage
    /// and IOVA still reachable by the device. A translated backend must
    /// avoid mapping borrowed source pages directly unless invalidation
    /// cannot fail while those pages are reachable: safe streaming mappings
    /// release the source borrow after drop, including this error path.
    ///
    /// # Safety
    ///
    /// Must be paired with `map_streaming`; device access has stopped.
    unsafe fn try_unmap_streaming(&self, handle: DmaMapHandle) -> Result<(), DmaError> {
        unsafe { self.unmap_streaming(handle) };
        Ok(())
    }

    fn flush(&self, addr: NonNull<u8>, size: usize) {
        mb();
        arch::flush(addr, size)
    }

    fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        arch::invalidate(addr, size);
        mb();
    }

    fn flush_invalidate(&self, addr: NonNull<u8>, size: usize) {
        mb();
        arch::flush_invalidate(addr, size);
        mb();
    }

    fn sync_alloc_for_device(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if matches!(
            direction,
            DmaDirection::ToDevice | DmaDirection::Bidirectional
        ) {
            self.flush(unsafe { handle.as_ptr().add(offset) }, size);
        } else if matches!(direction, DmaDirection::FromDevice) {
            self.invalidate(unsafe { handle.as_ptr().add(offset) }, size);
        }
    }

    fn sync_alloc_for_cpu(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        if matches!(
            direction,
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ) {
            self.invalidate(unsafe { handle.as_ptr().add(offset) }, size);
        }
    }

    fn sync_map_for_device(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
        coherency: DmaCoherency,
    ) {
        let source = unsafe { handle.as_ptr().add(offset) };
        if let Some(map_virt) = handle.bounce_ptr()
            && map_virt != handle.as_ptr()
        {
            let target = unsafe { map_virt.add(offset) };
            if matches!(
                direction,
                DmaDirection::ToDevice | DmaDirection::Bidirectional
            ) {
                unsafe {
                    target
                        .as_ptr()
                        .copy_from_nonoverlapping(source.as_ptr(), size);
                }
                if coherency == DmaCoherency::NonCoherent {
                    self.flush(target, size);
                }
            } else if matches!(direction, DmaDirection::FromDevice)
                && coherency == DmaCoherency::NonCoherent
            {
                self.invalidate(target, size);
            }
            return;
        }

        if coherency == DmaCoherency::Coherent {
            return;
        }

        match direction {
            DmaDirection::ToDevice => self.flush(source, size),
            DmaDirection::FromDevice => self.invalidate(source, size),
            DmaDirection::Bidirectional => self.flush_invalidate(source, size),
        }
    }

    fn sync_map_for_cpu(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
        coherency: DmaCoherency,
    ) {
        if !matches!(
            direction,
            DmaDirection::FromDevice | DmaDirection::Bidirectional
        ) {
            return;
        }

        let target = unsafe { handle.as_ptr().add(offset) };
        if let Some(map_virt) = handle.bounce_ptr()
            && map_virt != handle.as_ptr()
        {
            let source = unsafe { map_virt.add(offset) };
            if coherency == DmaCoherency::NonCoherent {
                self.invalidate(source, size);
            }
            unsafe {
                target
                    .as_ptr()
                    .copy_from_nonoverlapping(source.as_ptr(), size);
            }
            return;
        }

        if coherency == DmaCoherency::NonCoherent {
            self.invalidate(target, size);
        }
    }
}
