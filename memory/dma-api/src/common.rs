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

unsafe impl Send for DmaAllocation {}

impl DmaAllocation {
    pub fn new_zero_coherent(os: &DeviceDma, layout: Layout) -> Result<Self, DmaError> {
        let handle = unsafe { os.alloc_coherent(layout) }?;
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
        let handle = unsafe { os.alloc_contiguous(layout) }?;
        unsafe {
            handle.as_ptr().write_bytes(0, handle.size());
        }

        Ok(Self {
            handle: Some(handle),
            device: os.clone(),
            kind: AllocationKind::Contiguous { direction },
        })
    }

    pub fn handle(&self) -> &DmaAllocHandle {
        self.handle
            .as_ref()
            .expect("live DMA allocation must retain its handle")
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        let handle = self.handle();
        unsafe { core::slice::from_raw_parts_mut(handle.as_ptr().as_ptr(), handle.size()) }
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

    pub fn try_release(&mut self) -> Result<(), DmaError> {
        let Some(handle) = self.handle.take() else {
            return Ok(());
        };
        if handle.size() == 0 {
            return Ok(());
        }

        unsafe {
            match self.kind {
                AllocationKind::Coherent => self.device.dealloc_coherent(handle),
                AllocationKind::Contiguous { .. } => {
                    self.device.dealloc_contiguous(handle);
                    Ok(())
                }
            }
        }
    }
}

impl Drop for DmaAllocation {
    fn drop(&mut self) {
        if let Err(err) = self.try_release() {
            log::error!("failed to release coherent DMA allocation; allocation quarantined: {err}");
        }
    }
}
