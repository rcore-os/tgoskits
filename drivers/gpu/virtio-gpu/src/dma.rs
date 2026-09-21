//! Private DMA allocation helper.
//!
//! `virtio_drivers::hal::Dma` is not part of the published 0.13.0 API, so this
//! crate owns a minimal RAII wrapper around [`Hal::dma_alloc`] and
//! [`Hal::dma_dealloc`] for the memory it hands to the device.

use core::{marker::PhantomData, ptr::NonNull};

use virtio_drivers::{BufferDirection, Hal, PAGE_SIZE, PhysAddr};

use crate::Error;

/// A physically contiguous, zeroed DMA region released on drop.
pub(crate) struct Dma<H: Hal> {
    paddr: PhysAddr,
    vaddr: NonNull<u8>,
    /// Bytes requested by the caller (the usable length).
    len: usize,
    /// Whole pages actually allocated, which is `len` rounded up.
    pages: usize,
    _hal: PhantomData<H>,
}

// SAFETY: the value owns the allocation. Moving that ownership, and the
// physical address that names it, to another thread keeps every invariant the
// `Hal` contract established; reaching the bytes still requires `&mut self`.
unsafe impl<H: Hal> Send for Dma<H> {}

// SAFETY: `&Dma` exposes only the physical address, the base pointer and a raw
// slice, none of which read or write the region. Any actual access is unsafe
// code that has to arrange its own freedom from data races, exactly as it would
// without this impl.
unsafe impl<H: Hal> Sync for Dma<H> {}

impl<H: Hal> Dma<H> {
    /// Allocates at least `len` zeroed bytes of physically contiguous DMA memory.
    ///
    /// The allocation is rounded up to whole pages, never down, so the returned
    /// region is always at least as large as `len` bytes.
    pub(crate) fn new(len: usize, direction: BufferDirection) -> Result<Self, Error> {
        let pages = page_count(len)?;
        let (paddr, vaddr) = H::dma_alloc(pages, direction);
        // A zero physical address is how the `Hal` contract reports a failed
        // allocation; the pointer itself is a `NonNull`, so it cannot be null.
        if paddr == 0 {
            return Err(Error::DmaError);
        }
        Ok(Self {
            paddr,
            vaddr,
            len,
            pages,
            _hal: PhantomData,
        })
    }

    /// Physical address of the region, as seen by the device.
    pub(crate) fn paddr(&self) -> PhysAddr {
        self.paddr
    }

    /// The requested region as a raw slice of exactly `len` bytes.
    ///
    /// The allocation itself is rounded up to whole pages, but the extra
    /// page-alignment tail is padding: exposing it would make callers (such as
    /// the framebuffer path) treat it as usable memory. Only `len` bytes are
    /// returned, which is the count the caller asked for and the count the
    /// device is told about.
    pub(crate) fn raw_slice(&self) -> NonNull<[u8]> {
        NonNull::slice_from_raw_parts(self.vaddr, self.len)
    }
}

impl<H: Hal> Drop for Dma<H> {
    fn drop(&mut self) {
        // SAFETY: the region was allocated by `H::dma_alloc` in `new`, has not
        // been deallocated since, and `paddr`, `vaddr` and `pages` are exactly
        // the values that allocation returned.
        let result = unsafe { H::dma_dealloc(self.paddr, self.vaddr, self.pages) };
        debug_assert_eq!(result, 0, "failed to deallocate DMA memory");
    }
}

/// Rounds `len` bytes up to a whole number of [`PAGE_SIZE`] pages.
fn page_count(len: usize) -> Result<usize, Error> {
    if len == 0 {
        return Err(Error::InvalidParam);
    }
    // `len + PAGE_SIZE - 1` rounded up can overflow for lengths near
    // `usize::MAX`; reject instead of wrapping into a too-small allocation.
    let rounded = len.checked_add(PAGE_SIZE - 1).ok_or(Error::Overflow)?;
    Ok(rounded / PAGE_SIZE)
}
