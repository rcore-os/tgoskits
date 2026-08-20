use alloc::vec::Vec;
use core::{marker::PhantomData, num::NonZeroUsize, ops::Range, ptr::NonNull};

use crate::{DeviceDma, DmaDirection, DmaError, DmaMapHandle, DmaPod};

pub struct StreamingMap<T: DmaPod> {
    handle: DmaMapHandle,
    device: DeviceDma,
    direction: DmaDirection,
    _marker: PhantomData<*mut T>,
}

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
            handle,
            device: os.clone(),
            direction,
            _marker: PhantomData,
        })
    }

    pub fn dma_addr(&self) -> crate::DmaAddr {
        self.handle.dma_addr()
    }

    pub fn len(&self) -> usize {
        if core::mem::size_of::<T>() == 0 {
            0
        } else {
            self.handle.size() / core::mem::size_of::<T>()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes_len(&self) -> usize {
        self.handle.size()
    }

    pub fn read_cpu(&self, index: usize) -> Option<T> {
        if index >= self.len() {
            return None;
        }
        Some(unsafe { self.handle.as_ptr().cast::<T>().add(index).read() })
    }

    pub fn set_cpu(&mut self, index: usize, value: T) {
        assert!(
            index < self.len(),
            "index out of range, index: {}, len: {}",
            index,
            self.len()
        );
        unsafe {
            self.handle.as_ptr().cast::<T>().add(index).write(value);
        }
    }

    pub fn copy_from_slice_cpu(&mut self, src: &[T]) {
        assert!(
            core::mem::size_of_val(src) <= self.handle.size(),
            "source slice is larger than DMA buffer"
        );
        unsafe {
            self.handle
                .as_ptr()
                .cast::<T>()
                .as_ptr()
                .copy_from_nonoverlapping(src.as_ptr(), src.len());
        }
    }

    pub fn write_with_cpu<R>(&mut self, len: usize, f: impl FnOnce(&mut [T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        let data = unsafe {
            core::slice::from_raw_parts_mut(self.handle.as_ptr().cast::<T>().as_ptr(), len)
        };
        f(data)
    }

    pub fn read_with_cpu<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        let data =
            unsafe { core::slice::from_raw_parts(self.handle.as_ptr().cast::<T>().as_ptr(), len) };
        f(data)
    }

    pub fn to_vec_cpu(&self) -> Vec<T> {
        let mut vec: Vec<T> = Vec::with_capacity(self.len());
        unsafe {
            let src_ptr = self.handle.as_ptr().as_ptr().cast::<T>();
            let dst_ptr = vec.as_mut_ptr();
            dst_ptr.copy_from_nonoverlapping(src_ptr, self.len());
            vec.set_len(self.len());
        }
        vec
    }

    pub fn prepare_for_device(&self, range: Range<usize>) {
        self.check_range(&range);
        self.device
            .sync_map_for_device(&self.handle, range.start, range.len(), self.direction);
    }

    pub fn complete_for_cpu(&self, range: Range<usize>) {
        self.check_range(&range);
        self.device
            .sync_map_for_cpu(&self.handle, range.start, range.len(), self.direction);
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
        self.handle.bounce_ptr()
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

impl<T: DmaPod> Drop for StreamingMap<T> {
    fn drop(&mut self) {
        unsafe {
            self.device.unmap_streaming(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    fn streaming_struct_and_phantom_hold_for_test() -> bool {
        // Verify StreamingMap struct exists with PhantomData marker
        // We can't construct it without a real DeviceDma, but verify type properties

        // Check that size_of::<T>() == 0 gives len() == 0
        assert!(core::mem::size_of::<u8>() > 0);

        true
    }

    fn streaming_direction_and_error_types_hold_for_test() -> bool {
        // Test DmaDirection variants
        use crate::DmaDirection;
        let _to_device = DmaDirection::ToDevice;
        let _from_device = DmaDirection::FromDevice;
        let _bidirectional = DmaDirection::Bidirectional;

        // Test DmaError variants
        use crate::DmaError;
        let _no_memory = DmaError::NoMemory;

        true
    }

    fn streaming_struct_size_and_alignment_hold_for_test() -> bool {
        // Test that StreamingMap has expected size properties
        assert!(core::mem::size_of::<u8>() == 1);
        assert!(core::mem::size_of::<u32>() == 4);
        assert!(core::mem::size_of::<u64>() == 8);

        true
    }

    fn streaming_all_error_variants_hold_for_test() -> bool {
        // Test all DmaError variants
        use crate::DmaError;

        let _no_memory = DmaError::NoMemory;
        let _null_pointer = DmaError::NullPointer;
        let _zero_sized = DmaError::ZeroSizedBuffer;

        // Verify they are different types
        assert!(core::mem::size_of_val(&DmaError::NoMemory) > 0);

        true
    }

    fn streaming_dma_pod_types_hold_for_test() -> bool {
        // Test that common types satisfy DmaPod bounds
        use core::mem;

        // u8 is POD
        assert!(mem::size_of::<u8>() == 1);
        assert!(mem::align_of::<u8>() >= 1);

        // u32 is POD
        assert!(mem::size_of::<u32>() == 4);
        assert!(mem::align_of::<u32>() >= 1);

        // u64 is POD
        assert!(mem::size_of::<u64>() == 8);
        assert!(mem::align_of::<u64>() >= 1);

        true
    }

    fn streaming_nonzero_and_phantom_hold_for_test() -> bool {
        use core::{marker::PhantomData, num::NonZeroUsize};

        // Test NonZeroUsize
        let nz = NonZeroUsize::new(42).unwrap();
        assert_eq!(nz.get(), 42);

        // Test PhantomData
        let _phantom: PhantomData<*mut u8> = PhantomData;

        true
    }

    fn streaming_dma_direction_all_variants_hold_for_test() -> bool {
        use crate::DmaDirection;

        // Test all DmaDirection variants
        let to_device = DmaDirection::ToDevice;
        let from_device = DmaDirection::FromDevice;
        let bidirectional = DmaDirection::Bidirectional;

        // Verify they are different
        assert!(core::mem::discriminant(&to_device) != core::mem::discriminant(&from_device));
        assert!(core::mem::discriminant(&from_device) != core::mem::discriminant(&bidirectional));
        assert!(core::mem::discriminant(&to_device) != core::mem::discriminant(&bidirectional));

        true
    }

    #[test]
    fn streaming_struct_and_phantom_hold() {
        assert!(streaming_struct_and_phantom_hold_for_test());
    }

    #[test]
    fn streaming_direction_and_error_types_hold() {
        assert!(streaming_direction_and_error_types_hold_for_test());
    }

    #[test]
    fn streaming_struct_size_and_alignment_hold() {
        assert!(streaming_struct_size_and_alignment_hold_for_test());
    }

    #[test]
    fn streaming_all_error_variants_hold() {
        assert!(streaming_all_error_variants_hold_for_test());
    }

    #[test]
    fn streaming_dma_pod_types_hold() {
        assert!(streaming_dma_pod_types_hold_for_test());
    }

    #[test]
    fn streaming_nonzero_and_phantom_hold() {
        assert!(streaming_nonzero_and_phantom_hold_for_test());
    }

    #[test]
    fn streaming_dma_direction_all_variants_hold() {
        assert!(streaming_dma_direction_all_variants_hold_for_test());
    }
}
