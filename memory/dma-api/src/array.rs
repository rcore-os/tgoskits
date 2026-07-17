use core::{alloc::Layout, marker::PhantomData, ptr::NonNull};

use crate::{
    DeviceDma, DmaAddr, DmaDirection, DmaDomainId, DmaError, DmaPod,
    common::{AllocationKind, DmaAllocation},
};

pub struct CoherentArray<T: DmaPod> {
    data: DmaAllocation,
    _phantom: PhantomData<T>,
}

unsafe impl<T: DmaPod + Send> Send for CoherentArray<T> {}
unsafe impl<T: DmaPod + Sync> Sync for CoherentArray<T> {}

impl<T: DmaPod> CoherentArray<T> {
    pub(crate) fn new_zero_with_align(
        os: &DeviceDma,
        len: usize,
        align: usize,
    ) -> Result<Self, DmaError> {
        let layout = array_layout::<T>(len, align)?;
        Ok(Self {
            data: DmaAllocation::new_zero_coherent(os, layout)?,
            _phantom: PhantomData,
        })
    }

    pub(crate) fn new_zero(os: &DeviceDma, len: usize) -> Result<Self, DmaError> {
        Self::new_zero_with_align(os, len, core::mem::align_of::<T>())
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.data.handle().dma_addr()
    }

    pub fn len(&self) -> usize {
        len_from_bytes::<T>(self.data.handle().size())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes_len(&self) -> usize {
        self.data.handle().size()
    }

    pub fn read_cpu(&self, index: usize) -> Option<T> {
        read_at(self.as_ptr(), self.len(), index)
    }

    pub fn set_cpu(&mut self, index: usize, value: T) {
        write_at(self.as_ptr(), self.len(), index, value);
    }

    pub fn copy_from_slice_cpu(&mut self, src: &[T]) {
        copy_from_slice(self.as_ptr(), self.len(), src);
    }

    pub fn iter_cpu(&self) -> ArrayCpuIter<'_, T, Self> {
        ArrayCpuIter {
            array: self,
            index: 0,
            _phantom: PhantomData,
        }
    }

    pub fn write_with_cpu<R>(&mut self, len: usize, f: impl FnOnce(&mut [T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        let data = unsafe { self.as_mut_slice_cpu() };
        f(&mut data[..len])
    }

    pub fn read_with_cpu<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        let data = unsafe { core::slice::from_raw_parts(self.as_ptr().as_ptr(), len) };
        f(data)
    }

    pub fn as_ptr(&self) -> NonNull<T> {
        self.data.handle().as_ptr().cast::<T>()
    }

    pub fn as_slice_cpu(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.as_ptr().as_ptr(), self.len()) }
    }

    /// # Safety
    ///
    /// The caller must ensure the device is not concurrently accessing this
    /// memory in a way that races with CPU writes.
    pub unsafe fn as_mut_slice_cpu(&mut self) -> &mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.as_ptr().as_ptr(), self.len()) }
    }
}

pub struct ContiguousArray<T: DmaPod> {
    data: DmaAllocation,
    _phantom: PhantomData<T>,
}

unsafe impl<T: DmaPod + Send> Send for ContiguousArray<T> {}
unsafe impl<T: DmaPod + Sync> Sync for ContiguousArray<T> {}

impl<T: DmaPod> ContiguousArray<T> {
    pub(crate) fn new_zero_with_align(
        os: &DeviceDma,
        len: usize,
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let layout = array_layout::<T>(len, align)?;
        Ok(Self {
            data: DmaAllocation::new_zero_contiguous(os, layout, direction)?,
            _phantom: PhantomData,
        })
    }

    pub(crate) fn new_zero(
        os: &DeviceDma,
        len: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        Self::new_zero_with_align(os, len, core::mem::align_of::<T>(), direction)
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.data.handle().dma_addr()
    }

    pub fn len(&self) -> usize {
        len_from_bytes::<T>(self.data.handle().size())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn bytes_len(&self) -> usize {
        self.data.handle().size()
    }

    pub fn domain_id(&self) -> DmaDomainId {
        self.data.device().domain_id()
    }

    pub fn direction(&self) -> DmaDirection {
        match self.data.kind() {
            AllocationKind::Contiguous { direction } => direction,
            AllocationKind::Coherent => unreachable!("ContiguousArray cannot hold coherent DMA"),
        }
    }

    pub fn read_cpu(&self, index: usize) -> Option<T> {
        read_at(self.as_ptr(), self.len(), index)
    }

    pub fn set_cpu(&mut self, index: usize, value: T) {
        write_at(self.as_ptr(), self.len(), index, value);
    }

    pub fn copy_from_slice_cpu(&mut self, src: &[T]) {
        copy_from_slice(self.as_ptr(), self.len(), src);
    }

    pub fn iter_cpu(&self) -> ArrayCpuIter<'_, T, Self> {
        ArrayCpuIter {
            array: self,
            index: 0,
            _phantom: PhantomData,
        }
    }

    pub fn sync_for_device(&self, offset: usize, size: usize) {
        self.check_range(offset, size);
        self.data.sync_for_device(offset, size);
    }

    pub fn sync_for_cpu(&self, offset: usize, size: usize) {
        self.check_range(offset, size);
        self.data.sync_for_cpu(offset, size);
    }

    pub fn sync_for_device_all(&self) {
        self.data.sync_for_device(0, self.bytes_len());
    }

    pub fn sync_for_cpu_all(&self) {
        self.data.sync_for_cpu(0, self.bytes_len());
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
        let size = len * core::mem::size_of::<T>();
        self.complete_for_cpu(0, size);
        self.read_with_cpu(len, f)
    }

    pub fn copy_to_device_from_slice(&mut self, src: &[T]) {
        self.copy_from_slice_cpu(src);
        self.prepare_for_device(0, core::mem::size_of_val(src));
    }

    pub fn copy_from_device_to_slice(&self, dst: &mut [T]) {
        self.read_from_device(dst.len(), |src| dst.copy_from_slice(src));
    }

    pub fn write_with_cpu<R>(&mut self, len: usize, f: impl FnOnce(&mut [T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        {
            let data = unsafe { self.as_mut_slice_cpu() };
            f(&mut data[..len])
        }
    }

    pub fn read_with_cpu<R>(&self, len: usize, f: impl FnOnce(&[T]) -> R) -> R {
        assert!(len <= self.len(), "range out of bounds");
        let data = unsafe { core::slice::from_raw_parts(self.as_ptr().as_ptr(), len) };
        f(data)
    }

    pub fn as_ptr(&self) -> NonNull<T> {
        self.data.handle().as_ptr().cast::<T>()
    }

    pub fn as_slice_cpu(&self) -> &[T] {
        unsafe { core::slice::from_raw_parts(self.as_ptr().as_ptr(), self.len()) }
    }

    /// # Safety
    ///
    /// The caller must ensure the device is not concurrently accessing this
    /// memory in a way that races with CPU writes.
    pub unsafe fn as_mut_slice_cpu(&mut self) -> &mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.as_ptr().as_ptr(), self.len()) }
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

pub trait DmaArrayCpuRead<T: DmaPod> {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool;
    fn read_cpu(&self, index: usize) -> Option<T>;
}

impl<T: DmaPod> DmaArrayCpuRead<T> for CoherentArray<T> {
    fn len(&self) -> usize {
        CoherentArray::len(self)
    }

    fn is_empty(&self) -> bool {
        CoherentArray::is_empty(self)
    }

    fn read_cpu(&self, index: usize) -> Option<T> {
        CoherentArray::read_cpu(self, index)
    }
}

impl<T: DmaPod> DmaArrayCpuRead<T> for ContiguousArray<T> {
    fn len(&self) -> usize {
        ContiguousArray::len(self)
    }

    fn is_empty(&self) -> bool {
        ContiguousArray::is_empty(self)
    }

    fn read_cpu(&self, index: usize) -> Option<T> {
        ContiguousArray::read_cpu(self, index)
    }
}

pub struct ArrayCpuIter<'a, T: DmaPod, A: DmaArrayCpuRead<T>> {
    array: &'a A,
    index: usize,
    _phantom: PhantomData<T>,
}

impl<'a, T: DmaPod, A: DmaArrayCpuRead<T>> Iterator for ArrayCpuIter<'a, T, A> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.array.len() {
            return None;
        }
        let value = self.array.read_cpu(self.index);
        self.index += 1;
        value
    }
}

fn array_layout<T>(len: usize, align: usize) -> Result<Layout, DmaError> {
    let size = len
        .checked_mul(core::mem::size_of::<T>())
        .ok_or(DmaError::LayoutError(
            Layout::from_size_align(usize::MAX, 1).unwrap_err(),
        ))?;
    Ok(Layout::from_size_align(
        size,
        align.max(core::mem::align_of::<T>()),
    )?)
}

fn len_from_bytes<T>(bytes: usize) -> usize {
    if core::mem::size_of::<T>() == 0 {
        0
    } else {
        bytes / core::mem::size_of::<T>()
    }
}

fn read_at<T: DmaPod>(ptr: NonNull<T>, len: usize, index: usize) -> Option<T> {
    if index >= len {
        return None;
    }
    Some(unsafe { ptr.add(index).read() })
}

fn write_at<T: DmaPod>(ptr: NonNull<T>, len: usize, index: usize, value: T) {
    assert!(
        index < len,
        "index out of range, index: {}, len: {}",
        index,
        len
    );
    unsafe { ptr.add(index).write(value) };
}

fn copy_from_slice<T: DmaPod>(ptr: NonNull<T>, len: usize, src: &[T]) {
    assert!(
        src.len() <= len,
        "source slice is larger than DMA array, src len: {}, array len: {}",
        src.len(),
        len
    );
    unsafe {
        ptr.as_ptr()
            .copy_from_nonoverlapping(src.as_ptr(), src.len());
    }
}
