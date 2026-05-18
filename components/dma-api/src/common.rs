use core::alloc::Layout;

use crate::{DeviceDma, DmaDirection, DmaError, DmaMapHandle};

pub(crate) struct DCommon {
    pub handle: DmaMapHandle,
    pub osal: DeviceDma,
    pub direction: DmaDirection,
}

unsafe impl Send for DCommon {}

impl DCommon {
    pub fn new_zero(
        os: &DeviceDma,
        layout: Layout,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let handle = unsafe { os.alloc_coherent(layout) }?;
        let ptr = handle.cpu_addr;
        unsafe {
            ptr.write_bytes(0, handle.size());
        }
        os.flush_invalidate(ptr, handle.size());

        Ok(Self {
            handle: DmaMapHandle {
                handle,
                map_alloc_virt: None,
            },
            osal: os.clone(),
            direction,
        })
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(self.handle.cpu_addr.as_ptr(), self.handle.size())
        }
    }

    pub fn prepare_read(&self, offset: usize, size: usize) {
        self.osal
            .prepare_read(&self.handle, offset, size, self.direction);
    }

    pub fn confirm_write(&self, offset: usize, size: usize) {
        self.osal
            .confirm_write(&self.handle, offset, size, self.direction);
    }

    pub fn confirm_write_all(&self) {
        self.osal
            .confirm_write(&self.handle, 0, self.handle.size(), self.direction);
    }
}

impl Drop for DCommon {
    fn drop(&mut self) {
        if self.handle.size() > 0 {
            unsafe {
                self.osal.dealloc_coherent(self.handle.handle);
            }
        }
    }
}
