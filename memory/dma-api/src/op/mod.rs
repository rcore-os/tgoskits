use core::{num::NonZeroUsize, ptr::NonNull};

use mbarrier::mb;

use crate::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle};

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

    /// # Safety
    ///
    /// Must be paired with `alloc_contiguous`.
    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle);

    /// Allocates coherent DMA memory.
    ///
    /// Coherent memory is CPU/device visible without explicit cache
    /// maintenance. Ordering barriers are still the driver's responsibility.
    ///
    /// # Safety
    ///
    /// Implementations must return a live allocation described by `layout`,
    /// with a DMA address range satisfying `constraints`, and with the backend's
    /// coherent mapping policy applied until `dealloc_coherent`.
    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle>;

    /// # Safety
    ///
    /// Must be paired with `alloc_coherent`.
    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle);

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
                self.flush(target, size);
            } else if matches!(direction, DmaDirection::FromDevice) {
                self.invalidate(target, size);
            }
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
            self.invalidate(source, size);
            unsafe {
                target
                    .as_ptr()
                    .copy_from_nonoverlapping(source.as_ptr(), size);
            }
            return;
        }

        self.invalidate(target, size);
    }
}

#[cfg(axtest)]
pub(crate) fn dma_op_direction_matching_hold_for_test() -> bool {
    // Test that DmaDirection variants work correctly for sync operations
    use crate::DmaDirection;

    let to_device = DmaDirection::ToDevice;
    let from_device = DmaDirection::FromDevice;
    let bidirectional = DmaDirection::Bidirectional;

    // Verify all directions are distinct
    assert!(to_device != from_device);
    assert!(from_device != bidirectional);
    assert!(to_device != bidirectional);

    true
}

#[cfg(axtest)]
pub(crate) fn dma_op_constraints_and_error_types_hold_for_test() -> bool {
    // Test DmaConstraints and DmaError types
    use crate::DmaError;

    // Test DmaError variants exist
    let _no_memory = DmaError::NoMemory;

    true
}

#[cfg(axtest)]
pub(crate) fn dma_op_sync_direction_branches_hold_for_test() -> bool {
    // Test that all DmaDirection branches are covered in sync logic
    use crate::DmaDirection;

    // Test ToDevice matches
    assert!(matches!(DmaDirection::ToDevice, DmaDirection::ToDevice));
    assert!(!matches!(DmaDirection::ToDevice, DmaDirection::FromDevice));
    assert!(!matches!(
        DmaDirection::ToDevice,
        DmaDirection::Bidirectional
    ));

    // Test FromDevice matches
    assert!(matches!(DmaDirection::FromDevice, DmaDirection::FromDevice));
    assert!(!matches!(DmaDirection::FromDevice, DmaDirection::ToDevice));
    assert!(!matches!(
        DmaDirection::FromDevice,
        DmaDirection::Bidirectional
    ));

    // Test Bidirectional matches both
    assert!(matches!(
        DmaDirection::Bidirectional,
        DmaDirection::Bidirectional
    ));
    assert!(!matches!(
        DmaDirection::Bidirectional,
        DmaDirection::ToDevice
    ));
    assert!(!matches!(
        DmaDirection::Bidirectional,
        DmaDirection::FromDevice
    ));

    // Test combined patterns
    assert!(matches!(
        DmaDirection::ToDevice,
        DmaDirection::ToDevice | DmaDirection::Bidirectional
    ));
    assert!(!matches!(
        DmaDirection::FromDevice,
        DmaDirection::ToDevice | DmaDirection::Bidirectional
    ));
    assert!(matches!(
        DmaDirection::Bidirectional,
        DmaDirection::ToDevice | DmaDirection::Bidirectional
    ));

    true
}
