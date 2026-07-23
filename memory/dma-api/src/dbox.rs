use core::{alloc::Layout, marker::PhantomData, ptr::NonNull};

use crate::{DeviceDma, DmaAddr, DmaDirection, DmaError, DmaPod, common::DmaAllocation};

pub struct CoherentBox<T: DmaPod> {
    data: DmaAllocation,
    _marker: PhantomData<T>,
}

// SAFETY: the allocation is uniquely owned and `T: Send`; moving the owner
// preserves the DMA allocation and release token.
unsafe impl<T: DmaPod + Send> Send for CoherentBox<T> {}
// SAFETY: shared CPU access only reads copied `T` values and requires
// `T: Sync`; mutable CPU access requires `&mut self`.
unsafe impl<T: DmaPod + Sync> Sync for CoherentBox<T> {}

impl<T: DmaPod> CoherentBox<T> {
    pub(crate) fn new_zero(os: &DeviceDma) -> Result<Self, DmaError> {
        Self::new_zero_with_align(os, core::mem::align_of::<T>())
    }

    pub(crate) fn new_zero_with_align(os: &DeviceDma, align: usize) -> Result<Self, DmaError> {
        let data = DmaAllocation::new_zero_coherent(os, box_layout::<T>(align)?)?;
        Ok(Self {
            data,
            _marker: PhantomData,
        })
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.data.handle().dma_addr()
    }

    pub fn read_cpu(&self) -> T {
        // SAFETY: the allocation is live and aligned for one initialized `T`;
        // `DmaPod` makes every device-written bit pattern valid to read.
        unsafe { self.as_ptr().read() }
    }

    pub fn write_cpu(&mut self, value: T) {
        // SAFETY: `&mut self` provides exclusive CPU access to the live,
        // correctly aligned allocation.
        unsafe { self.as_ptr().write(value) };
    }

    pub fn modify_cpu(&mut self, f: impl FnOnce(&mut T)) {
        let mut value = self.read_cpu();
        f(&mut value);
        self.write_cpu(value);
    }

    pub fn as_ptr(&self) -> NonNull<T> {
        self.data.handle().as_ptr().cast::<T>()
    }

    /// # Safety
    ///
    /// The caller must ensure the device is not concurrently accessing this
    /// memory in a way that races with CPU writes.
    pub unsafe fn as_bytes_mut_cpu(&mut self) -> &mut [u8] {
        self.data.as_mut_slice()
    }
}

pub struct ContiguousBox<T: DmaPod> {
    data: DmaAllocation,
    _marker: PhantomData<T>,
}

// SAFETY: the allocation is uniquely owned and `T: Send`; moving the owner
// preserves the DMA allocation and release token.
unsafe impl<T: DmaPod + Send> Send for ContiguousBox<T> {}
// SAFETY: shared CPU access only reads copied `T` values and requires
// `T: Sync`; cache ownership transitions do not expose mutable Rust aliases.
unsafe impl<T: DmaPod + Sync> Sync for ContiguousBox<T> {}

impl<T: DmaPod> ContiguousBox<T> {
    pub(crate) fn new_zero(os: &DeviceDma, direction: DmaDirection) -> Result<Self, DmaError> {
        Self::new_zero_with_align(os, core::mem::align_of::<T>(), direction)
    }

    pub(crate) fn new_zero_with_align(
        os: &DeviceDma,
        align: usize,
        direction: DmaDirection,
    ) -> Result<Self, DmaError> {
        let data = DmaAllocation::new_zero_contiguous(os, box_layout::<T>(align)?, direction)?;
        Ok(Self {
            data,
            _marker: PhantomData,
        })
    }

    pub fn dma_addr(&self) -> DmaAddr {
        self.data.handle().dma_addr()
    }

    pub fn read_cpu(&self) -> T {
        // SAFETY: the allocation is live and aligned for one initialized `T`;
        // `DmaPod` makes every device-written bit pattern valid to read.
        unsafe { self.as_ptr().read() }
    }

    pub fn write_cpu(&mut self, value: T) {
        // SAFETY: `&mut self` provides exclusive CPU access to the live,
        // correctly aligned allocation.
        unsafe { self.as_ptr().write(value) };
    }

    pub fn modify_cpu(&mut self, f: impl FnOnce(&mut T)) {
        let mut value = self.read_cpu();
        f(&mut value);
        self.write_cpu(value);
    }

    pub fn sync_for_device_all(&self) {
        self.data.sync_for_device(0, core::mem::size_of::<T>());
    }

    pub fn sync_for_cpu_all(&self) {
        self.data.sync_for_cpu(0, core::mem::size_of::<T>());
    }

    pub fn prepare_for_device_all(&self) {
        self.sync_for_device_all();
    }

    pub fn complete_for_cpu_all(&self) {
        self.sync_for_cpu_all();
    }

    pub fn write_for_device(&mut self, value: T) {
        self.write_cpu(value);
        self.prepare_for_device_all();
    }

    pub fn modify_for_device(&mut self, f: impl FnOnce(&mut T)) {
        self.modify_cpu(f);
        self.prepare_for_device_all();
    }

    pub fn read_from_device(&self) -> T {
        self.complete_for_cpu_all();
        self.read_cpu()
    }

    pub fn as_ptr(&self) -> NonNull<T> {
        self.data.handle().as_ptr().cast::<T>()
    }

    /// # Safety
    ///
    /// The caller must ensure the device is not concurrently accessing this
    /// memory in a way that races with CPU writes.
    pub unsafe fn as_bytes_mut_cpu(&mut self) -> &mut [u8] {
        self.data.as_mut_slice()
    }
}

fn box_layout<T>(align: usize) -> Result<Layout, DmaError> {
    Ok(Layout::from_size_align(
        core::mem::size_of::<T>(),
        align.max(core::mem::align_of::<T>()),
    )?)
}
