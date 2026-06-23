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

/// Where an RGA image's pixels live.
///
/// The RGA core programs a raw physical base address into the hardware (`ImageDesc::phys_addr`),
/// so it is backing-agnostic. This enum is the typed seam consumers use to distinguish a
/// driver-owned allocation from an externally-owned one (e.g. a dma-buf resolved from a userspace
/// fd). It carries no fd or OS types — the OS glue resolves an fd to `Imported { phys_addr, len }`.
pub enum RgaBufferBacking {
    /// Driver-owned contiguous DMA allocation (e.g. the selftest's scratch buffers).
    Owned(RgaDmaBuffer),
    /// Externally-owned buffer referenced by physical address. The caller guarantees the memory
    /// stays alive and cache-coherent (via the dma-buf sync ABI) for the operation's duration.
    Imported { phys_addr: u64, len: usize },
}

impl RgaBufferBacking {
    /// Device (bus) physical base address of the pixel data.
    pub fn phys_addr(&self) -> u64 {
        match self {
            Self::Owned(buf) => buf.phys_addr(),
            Self::Imported { phys_addr, .. } => *phys_addr,
        }
    }

    /// Byte length of the backing.
    pub fn len(&self) -> usize {
        match self {
            Self::Owned(buf) => buf.len(),
            Self::Imported { len, .. } => *len,
        }
    }

    /// Returns `true` if the backing has zero bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_backing_reports_phys_and_len() {
        let b = RgaBufferBacking::Imported {
            phys_addr: 0x4000_0000,
            len: 4096,
        };
        assert_eq!(b.phys_addr(), 0x4000_0000);
        assert_eq!(b.len(), 4096);
        assert!(!b.is_empty());
    }

    #[test]
    fn empty_imported_backing_is_empty() {
        let b = RgaBufferBacking::Imported {
            phys_addr: 0,
            len: 0,
        };
        assert!(b.is_empty());
    }
}
