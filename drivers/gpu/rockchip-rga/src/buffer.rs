//! Physically-contiguous DMA buffer for RGA image/command data.
//! Contiguous + direct physical addressing means neither the RGA2 local MMU nor a system IOMMU is
//! needed (see design doc §2 D3).
use dma_api::{ContiguousArray, DeviceDma, DmaDirection};

use crate::error::{Result, RgaError};

pub struct RgaDmaBuffer {
    inner: ContiguousArray<u8>,
    len: usize,
}

impl RgaDmaBuffer {
    pub fn alloc(dma: &DeviceDma, len: usize, direction: DmaDirection) -> Result<Self> {
        let inner = dma
            .contiguous_array_zero::<u8>(len, direction)
            .map_err(|_| RgaError::Dma)?;
        Ok(Self { inner, len })
    }

    pub fn phys_addr(&self) -> u64 {
        self.inner.dma_addr().as_u64()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn cpu_bytes(&self) -> &[u8] {
        self.inner.as_slice_cpu()
    }

    /// # Safety
    /// Caller must not retain the slice across a device submission.
    pub unsafe fn cpu_bytes_mut(&mut self) -> &mut [u8] {
        unsafe { self.inner.as_mut_slice_cpu() }
    }

    /// Hand ownership to the device before starting hardware (flush CPU writes).
    pub fn prepare_for_device(&self) {
        self.inner.prepare_for_device_all();
    }

    /// Reclaim ownership after completion (invalidate so the CPU sees device writes).
    pub fn complete_for_cpu(&self) {
        self.inner.complete_for_cpu_all();
    }
}
