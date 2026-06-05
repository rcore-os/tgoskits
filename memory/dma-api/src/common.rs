use core::alloc::Layout;

use crate::{DeviceDma, DmaAllocHandle, DmaDirection, DmaError};

pub(crate) enum AllocationKind {
    Coherent,
    Contiguous { direction: DmaDirection },
}

pub(crate) struct DmaAllocation {
    pub handle: DmaAllocHandle,
    pub device: DeviceDma,
    pub kind: AllocationKind,
}

unsafe impl Send for DmaAllocation {}

impl DmaAllocation {
    pub fn new_zero_coherent(os: &DeviceDma, layout: Layout) -> Result<Self, DmaError> {
        let handle = unsafe { os.alloc_coherent(layout) }?;
        unsafe {
            handle.as_ptr().write_bytes(0, handle.size());
        }

        Ok(Self {
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

        Ok(Self {
            handle,
            device: os.clone(),
            kind: AllocationKind::Contiguous { direction },
        })
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(self.handle.as_ptr().as_ptr(), self.handle.size())
        }
    }

    pub fn sync_for_device(&self, offset: usize, size: usize) {
        if let AllocationKind::Contiguous { direction } = self.kind {
            self.device
                .sync_alloc_for_device(&self.handle, offset, size, direction);
        }
    }

    pub fn sync_for_cpu(&self, offset: usize, size: usize) {
        if let AllocationKind::Contiguous { direction } = self.kind {
            self.device
                .sync_alloc_for_cpu(&self.handle, offset, size, direction);
        }
    }
}

impl Drop for DmaAllocation {
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
