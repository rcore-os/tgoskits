//! A minimal contiguous dma-buf file backing the `/dev/dma_heap` allocator.
//!
//! Each [`DmaBufFile`] owns one physically-contiguous, DMA-coherent allocation
//! (via `dma-api`). It is handed to userspace as a file descriptor; `mmap` maps
//! the buffer's physical pages, and the physical base is what the
//! `/dev/mpp_service` node programs into the JPEG decoder. The allocation lives
//! in an inner `Arc` so that an active `mmap` keeps the pages alive even if the
//! fd is closed first; it is freed only when both the fd and every mmap drop.

use alloc::{borrow::Cow, sync::Arc};
use core::{any::Any, ffi::c_int};

use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange};
use axpoll::{IoEvents, Pollable};
use dma_api::CoherentArray;
use linux_raw_sys::general::O_RDWR;

use super::{FileLike, Kstat};
use crate::pseudofs::DeviceMmap;

/// The owned contiguous allocation. Freed when the last reference (the fd's
/// `DmaBufFile` and any mmap retainer) drops.
struct DmaBufAlloc {
    dma: CoherentArray<u8>,
    size: usize,
}

/// A contiguous, DMA-coherent buffer exposed as a dma-buf file.
pub struct DmaBufFile {
    alloc: Arc<DmaBufAlloc>,
}

impl DmaBufFile {
    /// Allocate a page-aligned contiguous buffer of at least `len` bytes.
    pub fn alloc(len: usize) -> AxResult<Self> {
        let align = PAGE_SIZE_4K;
        let size = len
            .checked_next_multiple_of(align)
            .ok_or(AxError::InvalidInput)?
            .max(align);
        // The accelerators that consume these buffers (JPU/RGA/NPU) run with the
        // IOMMU bypassed and program raw 32-bit physical DMA addresses, so the
        // backing pages must live below 4 GiB. Plain `alloc_coherent_pages` draws
        // from anywhere in RAM and returns >4 GiB pages on large-memory boards,
        // which the 32-bit address registers cannot reach.
        let device = axklib::dma::device_with_mask(u32::MAX as u64);
        let dma = device
            .coherent_array_zero_with_align::<u8>(size, align)
            .map_err(|_| AxError::NoMemory)?;
        Ok(Self {
            alloc: Arc::new(DmaBufAlloc { dma, size }),
        })
    }

    /// Physical address range of the buffer.
    pub fn phys_range(&self) -> PhysAddrRange {
        PhysAddrRange::from_start_size(
            PhysAddr::from(self.alloc.dma.dma_addr().as_u64() as usize),
            self.alloc.size,
        )
    }

    /// Physical base address.
    pub fn phys_base(&self) -> usize {
        self.alloc.dma.dma_addr().as_u64() as usize
    }

    /// Size of the allocation in bytes (page-rounded up from the request).
    ///
    /// The NPU import seam ([`ContiguousDmaBuf`]) needs it, and the RGA path uses it to
    /// bound-check every imported buffer before an MMU-off DMA (a plane must not address
    /// past its buffer). The jpeg-only build resolves buffers through [`Self::phys_base`].
    #[cfg(any(feature = "rknpu", feature = "rga"))]
    pub fn size(&self) -> usize {
        self.alloc.size
    }
}

/// A physically-contiguous, device-reachable DMA buffer that the accelerator
/// dev-nodes (JPU / RGA / NPU) can share by fd for zero-copy. The RK3588 engines
/// run IOMMU-bypassed, so the physical base is exactly what they program into
/// their address registers.
///
/// Consumed by the NPU import path (card1 `PRIME_FD_TO_HANDLE`), which needs the
/// CPU base and a lifetime retainer; the jpeg-only `/dev/mpp_service` path uses
/// the inherent [`DmaBufFile::phys_base`] instead, so this is gated on `rknpu`.
#[cfg(feature = "rknpu")]
pub trait ContiguousDmaBuf {
    /// Device-reachable physical/bus base address.
    fn dma_phys_base(&self) -> usize;
    /// Allocation length in bytes.
    fn dma_size(&self) -> usize;
    /// Kernel CPU virtual base, if the buffer is CPU-mapped (the coherent heap is).
    fn dma_cpu_base(&self) -> Option<usize>;
    /// A type-erased owner whose lifetime keeps the pages alive; an importer
    /// stores it so the buffer cannot be freed while another engine references it.
    fn dma_retainer(&self) -> Arc<dyn Any + Send + Sync>;
}

#[cfg(feature = "rknpu")]
impl ContiguousDmaBuf for DmaBufFile {
    fn dma_phys_base(&self) -> usize {
        self.phys_base()
    }

    fn dma_size(&self) -> usize {
        self.size()
    }

    fn dma_cpu_base(&self) -> Option<usize> {
        Some(self.alloc.dma.as_ptr().as_ptr() as usize)
    }

    fn dma_retainer(&self) -> Arc<dyn Any + Send + Sync> {
        self.alloc.clone()
    }
}

/// Resolve a userspace dma-buf fd to its backing contiguous allocation. Returns
/// `None` if the fd is not one of our shareable contiguous buffers (e.g. a
/// socket, pipe, or regular file) — callers reject with `EINVAL`.
///
/// This is the single seam every accelerator node uses to turn an fd into a
/// physical address, so JPU / RGA / NPU all resolve shared buffers identically.
pub fn resolve_contiguous_dmabuf(fd: c_int) -> Option<Arc<DmaBufFile>> {
    let file = super::get_file_like(fd).ok()?;
    file.downcast_arc::<DmaBufFile>().ok()
}

impl Pollable for DmaBufFile {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: IoEvents) {}
}

impl FileLike for DmaBufFile {
    fn stat(&self) -> AxResult<Kstat> {
        Ok(Kstat {
            size: self.alloc.size as u64,
            ..Default::default()
        })
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed("/dev/dma_heap_buffer")
    }

    /// The buffer is read-write: `librockchip_mpp` gates `mmap` `PROT_WRITE` on
    /// `fcntl(fd, F_GETFL) & O_RDWR`, and it writes the stream and table buffers
    /// through the mapping, so the dma-buf fd must report read-write access.
    fn open_flags(&self) -> u32 {
        O_RDWR
    }

    fn device_mmap(&self, _offset: u64, _length: u64) -> AxResult<DeviceMmap> {
        // Retain the allocation for the lifetime of the mapping so the pages are
        // not freed if userspace closes the fd while it is still mapped.
        let retainer: Arc<dyn Any + Send + Sync> = self.alloc.clone();
        Ok(DeviceMmap::Physical(self.phys_range(), Some(retainer)))
    }
}
