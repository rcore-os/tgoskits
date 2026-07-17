use alloc::boxed::Box;
use core::alloc::Layout;

use crate::{DeviceDma, DmaAllocHandle, DmaDirection, DmaError};

#[derive(Clone, Copy)]
pub(crate) enum AllocationKind {
    Coherent,
    Contiguous { direction: DmaDirection },
}

pub(crate) struct DmaAllocation {
    inner: Box<DmaAllocationInner>,
}

struct DmaAllocationInner {
    handle: DmaAllocHandle,
    device: DeviceDma,
    kind: AllocationKind,
}

unsafe impl Send for DmaAllocation {}

impl DmaAllocation {
    pub fn new_zero_coherent(os: &DeviceDma, layout: Layout) -> Result<Self, DmaError> {
        let handle = unsafe { os.alloc_coherent(layout) }?;
        unsafe {
            handle.as_ptr().write_bytes(0, handle.size());
        }

        Self::try_from_inner(DmaAllocationInner {
            handle,
            device: os.clone(),
            kind: AllocationKind::Coherent,
        })
    }

    pub fn new_zero_contiguous(
        os: &DeviceDma,
        layout: Layout,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let handle = unsafe { os.alloc_contiguous(layout) }?;
        unsafe {
            handle.as_ptr().write_bytes(0, handle.size());
        }

        Self::try_from_inner(DmaAllocationInner {
            handle,
            device: os.clone(),
            kind: AllocationKind::Contiguous { direction },
        })
    }

    pub fn handle(&self) -> &DmaAllocHandle {
        &self.inner.handle
    }

    pub fn device(&self) -> &DeviceDma {
        &self.inner.device
    }

    pub const fn kind(&self) -> AllocationKind {
        self.inner.kind
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.inner.handle.as_ptr().as_ptr(),
                self.inner.handle.size(),
            )
        }
    }

    pub fn sync_for_device(&self, offset: usize, size: usize) {
        if let AllocationKind::Contiguous { direction } = self.inner.kind {
            self.inner
                .device
                .sync_alloc_for_device(&self.inner.handle, offset, size, direction);
        }
    }

    pub fn sync_for_cpu(&self, offset: usize, size: usize) {
        if let AllocationKind::Contiguous { direction } = self.inner.kind {
            self.inner
                .device
                .sync_alloc_for_cpu(&self.inner.handle, offset, size, direction);
        }
    }

    fn try_from_inner(inner: DmaAllocationInner) -> Result<Self, DmaError> {
        Box::try_new(inner)
            .map(|inner| Self { inner })
            .map_err(|_| DmaError::NoMemory)
    }
}

impl Drop for DmaAllocationInner {
    fn drop(&mut self) {
        if self.handle.size() == 0 {
            return;
        }
        unsafe {
            match self.kind {
                AllocationKind::Coherent => self.device.dealloc_coherent(self.handle),
                AllocationKind::Contiguous { .. } => self.device.dealloc_contiguous(self.handle),
            }
        }
    }
}
