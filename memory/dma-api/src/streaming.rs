use alloc::vec::Vec;
use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

use crate::{DeviceDma, DmaDirection, DmaError, DmaMapHandle, DmaPod};

pub struct StreamingMap<T: DmaPod> {
    handle: Option<DmaMapHandle>,
    device: DeviceDma,
    direction: DmaDirection,
    _marker: PhantomData<*mut T>,
}

// SAFETY: the mapping token is move-only, `T: Send`, and the borrowed buffer
// remains exclusively represented by this owner until unmap consumes it.
unsafe impl<T: DmaPod + Send> Send for StreamingMap<T> {}

impl<T: DmaPod> StreamingMap<T> {
    pub(crate) fn map(
        os: &DeviceDma,
        buff: &mut [T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let addr = NonNull::new(buff.as_mut_ptr().cast::<u8>()).ok_or(DmaError::NullPointer)?;
        let size =
            NonZeroUsize::new(core::mem::size_of_val(buff)).ok_or(DmaError::ZeroSizedBuffer)?;
        let handle = unsafe { os.map_streaming(addr, size, align, direction)? };

        Ok(Self {
            handle: Some(handle),
            device: os.clone(),
            direction,
            _marker: PhantomData,
        })
    }

    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.handle().dma_addr()
    }

    pub fn len(&self) -> usize {
        if core::mem::size_of::<T>() == 0 {
            0
        } else {
            self.handle().size() / core::mem::size_of::<T>()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes_len(&self) -> usize {
        self.handle().size()
    }

    pub fn read_cpu(&self, index: usize) -> Option<T> {
        if index >= self.len() {
            return None;
        }
        // SAFETY: the bounds check selects an initialized `T` inside the live
        // DMA mapping, and `DmaPod` permits every device-written bit pattern.
        Some(unsafe { self.handle().as_ptr().cast::<T>().add(index).read() })
    }

    pub fn set_cpu(&mut self, index: usize, value: T) {
        assert!(
            index < self.len(),
            "index out of range, index: {}, len: {}",
            index,
            self.len()
        );
        // SAFETY: the bounds check selects a writable `T` in the live mapping,
        // and `&mut self` prevents another CPU access through this owner.
        unsafe {
            self.handle().as_ptr().cast::<T>().add(index).write(value);
        }
    }

    pub fn copy_from_slice_cpu(&mut self, src: &[T]) {
        assert!(
            core::mem::size_of_val(src) <= self.handle().size(),
            "source slice is larger than DMA buffer"
        );
        // SAFETY: the size check bounds the non-overlapping copy to the live
        // allocation, which is exclusively borrowed through `&mut self`.
        unsafe {
            self.handle()
                .as_ptr()
                .cast::<T>()
                .as_ptr()
                .copy_from_nonoverlapping(src.as_ptr(), src.len());
        }
    }

    pub fn write_with_cpu<R>(&mut self, len: usize, f: impl FnOnce(&mut [T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        // SAFETY: `&mut self` provides exclusive CPU access, and `len` is
        // bounded by the initialized typed mapping.
        let data = unsafe {
            core::slice::from_raw_parts_mut(self.handle().as_ptr().cast::<T>().as_ptr(), len)
        };
        f(data)
    }

    pub fn read_with_cpu<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        // SAFETY: the mapping is live and `len` is bounded by its initialized
        // typed element count.
        let data = unsafe {
            core::slice::from_raw_parts(self.handle().as_ptr().cast::<T>().as_ptr(), len)
        };
        f(data)
    }

    pub fn to_vec_cpu(&self) -> Vec<T> {
        let mut vec: Vec<T> = Vec::with_capacity(self.len());
        // SAFETY: both ranges contain `self.len()` elements; `DmaPod` permits
        // copying device-written values into newly allocated vector storage.
        unsafe {
            let src_ptr = self.handle().as_ptr().as_ptr().cast::<T>();
            let dst_ptr = vec.as_mut_ptr();
            dst_ptr.copy_from_nonoverlapping(src_ptr, self.len());
            vec.set_len(self.len());
        }
        vec
    }

    pub fn sync_for_device(&self, offset: usize, size: usize) {
        self.check_range(offset, size);
        self.device
            .sync_map_for_device(self.handle(), offset, size, self.direction);
    }

    pub fn sync_for_cpu(&self, offset: usize, size: usize) {
        self.check_range(offset, size);
        self.device
            .sync_map_for_cpu(self.handle(), offset, size, self.direction);
    }

    pub fn sync_for_device_all(&self) {
        self.device
            .sync_map_for_device(self.handle(), 0, self.handle().size(), self.direction);
    }

    pub fn sync_for_cpu_all(&self) {
        self.device
            .sync_map_for_cpu(self.handle(), 0, self.handle().size(), self.direction);
    }

    pub fn prepare_for_device(&self, offset: usize, size: usize) {
        self.sync_for_device(offset, size);
    }

    pub fn prepare_for_device_all(&self) {
        self.sync_for_device_all();
    }

    pub fn complete_for_cpu(&self, offset: usize, size: usize) {
        self.sync_for_cpu(offset, size);
    }

    pub fn complete_for_cpu_all(&self) {
        self.sync_for_cpu_all();
    }

    pub fn write_for_device<R>(&mut self, len: usize, f: impl FnOnce(&mut [T]) -> R) -> R {
        let ret = self.write_with_cpu(len, f);
        self.prepare_for_device(0, len * core::mem::size_of::<T>());
        ret
    }

    pub fn read_from_device<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        self.complete_for_cpu(0, len * core::mem::size_of::<T>());
        self.read_with_cpu(len, f)
    }

    pub fn bounce_ptr(&self) -> Option<NonNull<u8>> {
        self.handle().bounce_ptr()
    }

    fn handle(&self) -> &DmaMapHandle {
        self.handle
            .as_ref()
            .expect("live DMA mapping must retain its backend token")
    }

    fn check_range(&self, offset: usize, size: usize) {
        assert!(
            offset <= self.bytes_len() && size <= self.bytes_len().saturating_sub(offset),
            "range out of bounds, offset: {}, size: {}, bytes_len: {}",
            offset,
            size,
            self.bytes_len()
        );
    }
}

impl<T: DmaPod> Drop for StreamingMap<T> {
    fn drop(&mut self) {
        let handle = self
            .handle
            .take()
            .expect("DMA mapping token must be consumed exactly once");
        // SAFETY: this owner removes and consumes the unique token returned by
        // the same device, after which no mapping state remains in `self`.
        unsafe {
            self.device.unmap_streaming(handle);
        }
    }
}
