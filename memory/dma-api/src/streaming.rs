use alloc::vec::Vec;
use core::{marker::PhantomData, num::NonZeroUsize, ops::Range, ptr::NonNull};

use crate::{DeviceDma, DmaDirection, DmaError, DmaMapHandle, DmaPod};

pub struct StreamingMap<'a, T: DmaPod> {
    handle: Option<DmaMapHandle>,
    device: DeviceDma,
    direction: DmaDirection,
    _borrow: PhantomData<&'a mut [T]>,
}

// SAFETY: the mutable source borrow (or the raw constructor's explicit
// lifetime obligation) moves with this mapping; DmaOp must synchronize device
// ownership before access and before releasing the mapping.
unsafe impl<T: DmaPod + Send> Send for StreamingMap<'_, T> {}

impl<'a, T: DmaPod> StreamingMap<'a, T> {
    pub(crate) fn map(
        os: &DeviceDma,
        buff: &'a mut [T],
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let len = buff.len();
        // SAFETY: the returned map holds the exclusive borrow of `buff`
        // until its destructor synchronously unmaps it.
        unsafe { Self::map_raw(os, NonNull::from(buff).cast::<T>(), len, align, direction) }
    }

    pub(crate) unsafe fn map_raw(
        os: &DeviceDma,
        addr: NonNull<T>,
        len: usize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let bytes = len
            .checked_mul(core::mem::size_of::<T>())
            .and_then(NonZeroUsize::new)
            .ok_or(DmaError::ZeroSizedBuffer)?;
        let handle = unsafe { os.map_streaming(addr.cast(), bytes, align, direction)? };
        Ok(Self {
            handle: Some(handle),
            device: os.clone(),
            direction,
            _borrow: PhantomData,
        })
    }

    fn handle(&self) -> &DmaMapHandle {
        self.handle
            .as_ref()
            .expect("live streaming mapping must retain its handle")
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
        Some(unsafe { self.handle().as_ptr().cast::<T>().add(index).read() })
    }

    pub fn set_cpu(&mut self, index: usize, value: T) {
        assert!(
            index < self.len(),
            "index out of range, index: {}, len: {}",
            index,
            self.len()
        );
        unsafe {
            self.handle().as_ptr().cast::<T>().add(index).write(value);
        }
    }

    pub fn copy_from_slice_cpu(&mut self, src: &[T]) {
        assert!(
            core::mem::size_of_val(src) <= self.handle().size(),
            "source slice is larger than DMA buffer"
        );
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
        let data = unsafe {
            core::slice::from_raw_parts_mut(self.handle().as_ptr().cast::<T>().as_ptr(), len)
        };
        f(data)
    }

    pub fn read_with_cpu<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        let data = unsafe {
            core::slice::from_raw_parts(self.handle().as_ptr().cast::<T>().as_ptr(), len)
        };
        f(data)
    }

    pub fn to_vec_cpu(&self) -> Vec<T> {
        let mut vec: Vec<T> = Vec::with_capacity(self.len());
        unsafe {
            let src_ptr = self.handle().as_ptr().as_ptr().cast::<T>();
            let dst_ptr = vec.as_mut_ptr();
            dst_ptr.copy_from_nonoverlapping(src_ptr, self.len());
            vec.set_len(self.len());
        }
        vec
    }

    pub fn prepare_for_device(&self, range: Range<usize>) {
        self.check_range(&range);
        self.device
            .sync_map_for_device(self.handle(), range.start, range.len(), self.direction);
    }

    pub fn complete_for_cpu(&self, range: Range<usize>) {
        self.check_range(&range);
        self.device
            .sync_map_for_cpu(self.handle(), range.start, range.len(), self.direction);
    }

    pub fn write_for_device<R>(&mut self, len: usize, f: impl FnOnce(&mut [T]) -> R) -> R {
        let ret = self.write_with_cpu(len, f);
        self.prepare_for_device(0..len * core::mem::size_of::<T>());
        ret
    }

    pub fn read_from_device<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        self.complete_for_cpu(0..len * core::mem::size_of::<T>());
        self.read_with_cpu(len, f)
    }

    pub fn bounce_ptr(&self) -> Option<NonNull<u8>> {
        self.handle().bounce_ptr()
    }

    /// Ends the mapping and reports an IOMMU invalidation failure.
    ///
    /// The backend must quarantine any pages still reachable by the device
    /// before returning an error. The borrowed source buffer is released
    /// when this method returns.
    pub fn try_unmap(mut self) -> Result<(), DmaError> {
        let handle = self
            .handle
            .take()
            .expect("live streaming mapping must retain its handle");
        // SAFETY: this mapping exclusively owns the handle and the caller
        // has stopped device access before ending the DMA operation.
        unsafe { self.device.unmap_streaming(handle) }
    }

    fn check_range(&self, range: &Range<usize>) {
        assert!(
            range.start <= range.end && range.end <= self.bytes_len(),
            "range out of bounds, range: {:?}, bytes_len: {}",
            range,
            self.bytes_len()
        );
    }
}

impl<T: DmaPod> Drop for StreamingMap<'_, T> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take()
            && let Err(err) = unsafe { self.device.unmap_streaming(handle) }
        {
            log::error!("failed to unmap streaming DMA; mapping quarantined: {err}");
        }
    }
}
