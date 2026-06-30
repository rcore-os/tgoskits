//! A minimal contiguous dma-buf file backing the `/dev/dma_heap` allocator.
//!
//! Each [`DmaBufFile`] owns one physically-contiguous, DMA-coherent allocation
//! (via `ax_dma`). It is handed to userspace as a file descriptor; `mmap` maps
//! the buffer's physical pages, and the physical base is what the
//! `/dev/mpp_service` node programs into the JPEG decoder. The allocation lives
//! in an inner `Arc` so that an active `mmap` keeps the pages alive even if the
//! fd is closed first; it is freed only when both the fd and every mmap drop.

use alloc::{borrow::Cow, sync::Arc};
use core::{alloc::Layout, any::Any};

use ax_dma::DMAInfo;
use ax_errno::{AxError, AxResult};
use ax_memory_addr::{PAGE_SIZE_4K, PhysAddr, PhysAddrRange};
use axpoll::{IoEvents, Pollable};
use linux_raw_sys::general::O_RDWR;

use super::{FileLike, Kstat};
use crate::pseudofs::DeviceMmap;

/// The owned contiguous allocation. Freed when the last reference (the fd's
/// `DmaBufFile` and any mmap retainer) drops.
struct DmaBufAlloc {
    dma: DMAInfo,
    size: usize,
    align: usize,
}

// The buffer is DMA-coherent memory addressed by physical address; the contained
// CPU pointer is only touched (uniquely) in `Drop`.
unsafe impl Send for DmaBufAlloc {}
unsafe impl Sync for DmaBufAlloc {}

impl Drop for DmaBufAlloc {
    fn drop(&mut self) {
        if let Ok(layout) = Layout::from_size_align(self.size, self.align) {
            unsafe { ax_dma::dealloc_coherent_pages(self.dma, layout) };
        }
    }
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
        let layout = Layout::from_size_align(size, align).map_err(|_| AxError::InvalidInput)?;
        let dma = unsafe { ax_dma::alloc_coherent_pages(layout) }.map_err(|_| AxError::NoMemory)?;
        Ok(Self {
            alloc: Arc::new(DmaBufAlloc { dma, size, align }),
        })
    }

    /// Physical address range of the buffer.
    pub fn phys_range(&self) -> PhysAddrRange {
        PhysAddrRange::from_start_size(
            PhysAddr::from(self.alloc.dma.bus_addr.as_u64() as usize),
            self.alloc.size,
        )
    }

    /// Physical base address.
    pub fn phys_base(&self) -> usize {
        self.alloc.dma.bus_addr.as_u64() as usize
    }
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
