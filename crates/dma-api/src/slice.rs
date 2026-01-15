use core::ptr::NonNull;

use alloc::sync::Arc;

use crate::{DeviceDmaOps, Direction, DmaError, MapHandle};

pub struct DSliceSingle<'a, T> {
    c: DSliceSingleCommon<'a, T>,
}

impl<'a, T> DSliceSingle<'a, T> {
    pub(crate) fn new(
        dev: &Arc<dyn DeviceDmaOps>,
        data: &'a [T],
        direction: Direction,
    ) -> Result<Self, DmaError> {
        let c = DSliceSingleCommon::new(dev, data, direction)?;
        Ok(Self { c })
    }

    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.c.handle.dma_addr
    }

    pub fn len(&self) -> usize {
        self.c.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read(&self, index: usize) -> T {
        self.c.read(index)
    }

    pub fn to_slice(self) -> &'a [T] {
        self.c.prepare_read_all();
        unsafe {
            core::slice::from_raw_parts(self.c.handle.virt_addr.cast::<T>().as_ptr(), self.len())
        }
    }

    pub fn iter(&'a mut self) -> impl Iterator<Item = T> + 'a {
        DSliceSingleIter {
            slice: &self.c,
            index: 0,
        }
    }
}

struct DSliceSingleIter<'a, T> {
    slice: &'a DSliceSingleCommon<'a, T>,
    index: usize,
}

impl<T> Iterator for DSliceSingleIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.slice.len() {
            return None;
        }

        let value = self.slice.read(self.index);
        self.index += 1;
        Some(value)
    }
}

pub struct DSliceSingleMut<'a, T> {
    c: DSliceSingleCommon<'a, T>,
}

impl<'a, T> DSliceSingleMut<'a, T> {
    pub(crate) fn new(
        dev: &Arc<dyn DeviceDmaOps>,
        data: &'a mut [T],
        direction: Direction,
    ) -> Result<Self, DmaError> {
        let c = DSliceSingleCommon::new(dev, data, direction)?;
        Ok(Self { c })
    }

    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.c.handle.dma_addr
    }

    pub fn len(&self) -> usize {
        self.c.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read(&self, index: usize) -> T {
        self.c.read(index)
    }

    pub fn write(&mut self, index: usize, value: T) {
        self.c.write(index, value);
    }

    pub fn iter(&'a mut self) -> impl Iterator<Item = T> + 'a {
        DSliceSingleIter {
            slice: &self.c,
            index: 0,
        }
    }

    pub fn copy_from_slice(&mut self, src: &[T]) {
        assert!(
            src.len() <= self.len(),
            "source slice length {} exceeds destination length {}",
            src.len(),
            self.len()
        );

        unsafe {
            let dest_ptr = self.c.handle.virt_addr.cast::<T>().as_ptr();
            core::ptr::copy_nonoverlapping(src.as_ptr(), dest_ptr, src.len());

            self.c.confirm_write_all();
        }
    }
}

struct DSliceSingleCommon<'a, T> {
    handle: MapHandle,
    direction: Direction,
    dev: Arc<dyn DeviceDmaOps>,
    _phantom: core::marker::PhantomData<&'a T>,
}

unsafe impl<T: Send> Send for DSliceSingleCommon<'_, T> {}

impl<'a, T> Drop for DSliceSingleCommon<'a, T> {
    fn drop(&mut self) {
        unsafe {
            self.dev.unmap_single(self.handle);
        }
    }
}

impl<'a, T> DSliceSingleCommon<'a, T> {
    fn new(
        dev: &Arc<dyn DeviceDmaOps>,
        s: &'a [T],
        direction: Direction,
    ) -> Result<Self, DmaError> {
        let size = size_of_val(s);
        let addr = NonNull::new(s.as_ptr() as usize as *mut u8).unwrap();
        let handle = unsafe { dev.map_single(addr, size, direction) }?;
        if dev.dma_mask() < handle.dma_addr + size as u64 {
            unsafe {
                dev.unmap_single(handle);
            }
            return Err(DmaError::DmaMaskNotMatch {
                addr: handle.dma_addr,
                mask: dev.dma_mask(),
            });
        }

        Ok(Self {
            handle,
            dev: dev.clone(),
            direction,
            _phantom: core::marker::PhantomData,
        })
    }

    fn len(&self) -> usize {
        self.handle.size / size_of::<T>()
    }

    fn read(&self, index: usize) -> T {
        assert!(index < self.len());

        let ptr = unsafe { self.handle.virt_addr.cast::<T>().add(index) };

        self.dev
            .prepare_read(ptr.cast(), size_of::<T>(), self.direction);

        unsafe { ptr.read_volatile() }
    }

    fn write(&self, index: usize, value: T) {
        assert!(index < self.len());

        let ptr = unsafe { self.handle.virt_addr.cast::<T>().add(index) };

        unsafe {
            ptr.write_volatile(value);
        }

        self.dev
            .confirm_write(ptr.cast(), size_of::<T>(), self.direction);
    }

    fn prepare_read_all(&self) {
        self.dev.prepare_read(
            self.handle.virt_addr.cast(),
            self.handle.size,
            self.direction,
        );
    }

    fn confirm_write_all(&self) {
        self.dev.confirm_write(
            self.handle.virt_addr.cast(),
            self.handle.size,
            self.direction,
        );
    }
}
