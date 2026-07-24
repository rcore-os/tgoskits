use core::alloc::Layout;

use crate::{DeviceDma, DmaAllocHandle, DmaDirection, DmaError};

#[derive(Clone, Copy)]
pub(crate) enum AllocationKind {
    Coherent,
    Contiguous { direction: DmaDirection },
}

pub(crate) struct DmaAllocation {
    handle: Option<DmaAllocHandle>,
    pub device: DeviceDma,
    pub kind: AllocationKind,
}

// SAFETY: the allocation token has unique ownership and can only be released
// by `Drop`; `DeviceDma` is backed by a `Sync` operation capability. Moving the
// owner between CPUs does not create another CPU or device access path.
unsafe impl Send for DmaAllocation {}

impl DmaAllocation {
    pub fn new_zero_coherent(os: &DeviceDma, layout: Layout) -> Result<Self, DmaError> {
        // SAFETY: the returned move-only token is immediately stored in this
        // owner and is consumed exactly once by `Drop`.
        let handle = unsafe { os.alloc_coherent(layout) }?;
        // SAFETY: the backend token describes a live writable allocation of
        // exactly `handle.size()` bytes, exclusively owned here.
        unsafe {
            handle.as_ptr().write_bytes(0, handle.size());
        }

        Ok(Self {
            handle: Some(handle),
            device: os.clone(),
            kind: AllocationKind::Coherent,
        })
    }

    pub fn new_zero_contiguous(
        os: &DeviceDma,
        layout: Layout,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        // SAFETY: the returned move-only token is immediately stored in this
        // owner and is consumed exactly once by `Drop`.
        let handle = unsafe { os.alloc_contiguous(layout) }?;
        // SAFETY: the backend token describes a live writable allocation of
        // exactly `handle.size()` bytes, exclusively owned here.
        unsafe {
            handle.as_ptr().write_bytes(0, handle.size());
        }

        Ok(Self {
            handle: Some(handle),
            device: os.clone(),
            kind: AllocationKind::Contiguous { direction },
        })
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let handle = self.handle();
        // SAFETY: `&mut self` gives exclusive CPU access to the live allocation
        // and the slice length comes from its allocation token.
        unsafe { core::slice::from_raw_parts_mut(handle.as_ptr().as_ptr(), handle.size()) }
    }

    pub fn handle(&self) -> &DmaAllocHandle {
        self.handle
            .as_ref()
            .expect("live DMA allocation must retain its backend token")
    }

    pub fn sync_for_device(&self, offset: usize, size: usize) {
        if let AllocationKind::Contiguous { direction } = self.kind {
            self.device
                .sync_alloc_for_device(self.handle(), offset, size, direction);
        }
    }

    pub fn sync_for_cpu(&self, offset: usize, size: usize) {
        if let AllocationKind::Contiguous { direction } = self.kind {
            self.device
                .sync_alloc_for_cpu(self.handle(), offset, size, direction);
        }
    }
}

impl Drop for DmaAllocation {
    fn drop(&mut self) {
        let handle = self
            .handle
            .take()
            .expect("DMA allocation token must be consumed exactly once");
        // SAFETY: the move-only token came from this device and is removed from
        // the owner before the matching backend release operation.
        unsafe {
            match self.kind {
                AllocationKind::Coherent => self.device.dealloc_coherent(handle),
                AllocationKind::Contiguous { .. } => self.device.dealloc_contiguous(handle),
            }
        }
    }
}
