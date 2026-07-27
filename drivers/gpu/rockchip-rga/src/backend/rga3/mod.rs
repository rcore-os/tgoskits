//! RGA3 backend skeleton. RGA3 uses a different task/register layout and the shared
//! `rockchip,iommu-v2` for non-contiguous buffers (design doc §2 D3). Contiguous bring-up is NOT
//! IOMMU-gated and is planned for the PR right after this one — pending an MMU-bypass confirmation.
pub mod registers;

use core::ptr::NonNull;

use dma_api::DeviceDma;

use crate::{
    RgaHardwareVersion, RgaVersion,
    backend::{RgaBackend, RgaStatus},
    error::{Result, RgaError},
    operation::RgaOperation,
};

/// RGA3 core controller — skeleton only in PR-1 (no submission path yet).
pub struct Rga3Backend {
    base: NonNull<u8>,
    _dma: DeviceDma,
}

// SAFETY: `base` is an MMIO region owned by this backend; access is serialized through `&mut self`.
unsafe impl Send for Rga3Backend {}

impl Rga3Backend {
    pub fn new(base: NonNull<u8>, dma: DeviceDma) -> Self {
        Self { base, _dma: dma }
    }

    fn read32(&self, off: usize) -> u32 {
        // SAFETY: `off` is a valid in-range register offset; `base` is a mapped MMIO region.
        unsafe { self.base.as_ptr().add(off).cast::<u32>().read_volatile() }
    }
}

impl RgaBackend for Rga3Backend {
    fn generation(&self) -> RgaVersion {
        RgaVersion::Rga3
    }

    fn read_version(&self) -> RgaHardwareVersion {
        // TODO(rga3): the RGA3 version register offset differs; 0x0028 is a best-effort read for now.
        let raw = self.read32(0x0028);
        RgaHardwareVersion {
            raw,
            major: ((raw >> 24) & 0xff) as u8,
            minor: ((raw >> 20) & 0x0f) as u8,
        }
    }

    fn supports(&self, _op: &RgaOperation) -> Result<()> {
        Err(RgaError::Unsupported)
    }

    fn submit(&mut self, _op: &RgaOperation) -> Result<()> {
        Err(RgaError::Unsupported)
    }

    fn poll(&self) -> RgaStatus {
        RgaStatus::Error
    }

    fn ack(&mut self) {}

    fn reset(&mut self) -> Result<()> {
        Err(RgaError::Unsupported)
    }
}
