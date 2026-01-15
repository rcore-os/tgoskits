use core::ops::Index;

use alloc::sync::Arc;

use crate::{DeviceDmaOps, Direction, DmaError, common::DCommon};

pub struct DArray<T> {
    data: DCommon<T>,
}

unsafe impl<T> Send for DArray<T> where T: Send {}

impl<T> DArray<T> {
    pub(crate) fn new_zero(
        os: &Arc<dyn DeviceDmaOps>,
        size: usize,
        align: usize,
        direction: Direction,
    ) -> Result<Self, DmaError> {
        let mut data = DCommon::new(os, size * core::mem::size_of::<T>(), align, direction)?;
        data.as_mut_slice().fill(0);
        data.confirm_write_all();
        Ok(Self { data })
    }

    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.data.handle.dma_addr
    }

    pub fn len(&self) -> usize {
        self.data.handle.size() / core::mem::size_of::<T>()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn read(&self, index: usize) -> Option<T> {
        if index >= self.len() {
            return None;
        }

        unsafe {
            let ptr = self.data.handle.virt_addr.cast::<T>().add(index);
            self.data.prepare_read(ptr.cast(), size_of::<T>());
            Some(ptr.read_volatile())
        }
    }

    pub fn set(&mut self, index: usize, value: T) {
        assert!(
            index < self.len(),
            "index out of range, index: {},len: {}",
            index,
            self.len()
        );

        unsafe {
            let ptr = self.data.handle.virt_addr.cast::<T>().add(index);
            ptr.write_volatile(value);
            self.data.confirm_write(ptr.cast(), size_of::<T>());
        }
    }

    pub fn iter(&self) -> DArrayIter<'_, T> {
        DArrayIter {
            array: self,
            index: 0,
        }
    }

    pub fn copy_from_slice(&mut self, src: &[T]) {
        assert!(
            src.len() <= self.len(),
            "source slice is larger than DArray, src len: {}, DArray len: {}",
            src.len(),
            self.len()
        );
        let src_bytes = unsafe {
            core::slice::from_raw_parts(src.as_ptr() as *const u8, core::mem::size_of_val(src))
        };
        self.data.as_mut_slice().copy_from_slice(src_bytes);
        self.data.confirm_write_all();
    }
}

pub struct DArrayIter<'a, T> {
    array: &'a DArray<T>,
    index: usize,
}

impl<'a, T> Iterator for DArrayIter<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.array.len() {
            return None;
        }
        let value = self.array.read(self.index);
        self.index += 1;
        value
    }
}

impl<T: Copy> Index<usize> for DArray<T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        assert!(
            index < self.len(),
            "index out of range, index: {},len: {}",
            index,
            self.len()
        );
        unsafe {
            let ptr = self.data.handle.virt_addr.cast::<T>().add(index);
            self.data.prepare_read(ptr.cast(), size_of::<T>());
            &*ptr.as_ptr()
        }
    }
}
