use alloc::sync::Arc;

use crate::{DeviceDmaOps, Direction, DmaError, common::DCommon};

pub struct DBox<T> {
    data: DCommon<T>,
}

impl<T> DBox<T> {
    pub(crate) fn new_zero(
        os: &Arc<dyn DeviceDmaOps>,
        align: usize,
        direction: Direction,
    ) -> Result<Self, DmaError> {
        let mut data = DCommon::new(os, core::mem::size_of::<T>(), align, direction)?;
        data.as_mut_slice().fill(0);
        data.confirm_write_all();
        Ok(Self { data })
    }

    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.data.handle.dma_addr
    }

    pub fn read(&self) -> T {
        unsafe {
            let ptr = self.data.handle.virt_addr.cast::<T>();
            self.data.prepare_read(ptr.cast(), size_of::<T>());
            ptr.read_volatile()
        }
    }

    pub fn write(&mut self, value: T) {
        unsafe {
            let ptr = self.data.handle.virt_addr.cast::<T>();
            ptr.write_volatile(value);
            self.data.confirm_write(ptr.cast(), size_of::<T>());
        }
    }

    pub fn modify(&mut self, f: impl FnOnce(&mut T)) {
        let mut value = self.read();
        f(&mut value);
        self.write(value);
    }
}
