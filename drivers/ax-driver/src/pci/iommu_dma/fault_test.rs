//! Deterministic failure injection run inside the ArceOS IOMMU QEMU case.

use alloc::sync::Arc;
use core::{
    alloc::Layout,
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
use rdif_iommu::{DmaDomainId, IommuDomain, IommuError, IovaWindow, MapPermissions};

use super::{IommuDma, PAGE_SIZE};

static PHYSICAL: CountingPhysical = CountingPhysical {
    released: AtomicUsize::new(0),
};
static DIRTY_PHYSICAL: DirtyPhysical = DirtyPhysical;

/// Returns live DMA pages filled with a recognizable previous-owner value.
struct DirtyPhysical;

impl DmaOp for DirtyPhysical {
    fn page_size(&self) -> usize {
        PAGE_SIZE
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        // SAFETY: this wrapper preserves the allocation contract of axklib.
        let handle = unsafe { axklib::dma::op().alloc_contiguous(constraints, layout) }?;
        // SAFETY: the returned handle owns all bytes in `layout`.
        unsafe { handle.as_ptr().write_bytes(0xa5, layout.size()) };
        Some(handle)
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        // SAFETY: the handle was allocated by the delegated axklib backend.
        unsafe { axklib::dma::op().dealloc_contiguous(handle) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        // SAFETY: this wrapper preserves the allocation contract of axklib.
        let handle = unsafe { axklib::dma::op().alloc_coherent(constraints, layout) }?;
        // SAFETY: the returned coherent alias covers all bytes in `layout`.
        unsafe { handle.as_ptr().write_bytes(0xa5, layout.size()) };
        Some(handle)
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        // SAFETY: the handle was allocated by the delegated axklib backend.
        unsafe { axklib::dma::op().dealloc_coherent(handle) }
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        Err(DmaError::MappingFailed)
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

struct CountingPhysical {
    released: AtomicUsize,
}

impl DmaOp for CountingPhysical {
    fn page_size(&self) -> usize {
        PAGE_SIZE
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { axklib::dma::op().alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { axklib::dma::op().dealloc_contiguous(handle) };
        self.released.fetch_add(1, Ordering::SeqCst);
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { axklib::dma::op().alloc_coherent(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        unsafe { axklib::dma::op().dealloc_coherent(handle) }?;
        self.released.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: DmaConstraints,
        _addr: NonNull<u8>,
        _size: NonZeroUsize,
        _direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        Err(DmaError::MappingFailed)
    }

    unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
}

struct FailingDomain {
    id: u64,
    window: IovaWindow,
    map_calls: AtomicUsize,
    mapped_pages: AtomicUsize,
    unmap_calls: AtomicUsize,
    fail_second_map: bool,
    fail_unmap: AtomicBool,
}

impl FailingDomain {
    fn new(id: u64, window: IovaWindow, fail_second_map: bool) -> Self {
        Self {
            id,
            window,
            map_calls: AtomicUsize::new(0),
            mapped_pages: AtomicUsize::new(0),
            unmap_calls: AtomicUsize::new(0),
            fail_second_map,
            fail_unmap: AtomicBool::new(false),
        }
    }
}

impl IommuDomain for FailingDomain {
    fn id(&self) -> DmaDomainId {
        DmaDomainId(self.id)
    }

    fn window(&self) -> IovaWindow {
        self.window
    }

    fn map_pages(
        &self,
        _iova: u64,
        _physical: u64,
        len: usize,
        _permissions: MapPermissions,
    ) -> Result<(), IommuError> {
        let call = self.map_calls.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_second_map && call == 2 {
            return Err(IommuError::OutOfMemory);
        }
        self.mapped_pages
            .fetch_add(len / PAGE_SIZE, Ordering::SeqCst);
        Ok(())
    }

    fn unmap_and_sync(&self, _iova: u64, len: usize) -> Result<(), IommuError> {
        self.unmap_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_unmap.load(Ordering::SeqCst) {
            return Err(IommuError::CommandTimeout);
        }
        self.mapped_pages
            .fetch_sub(len / PAGE_SIZE, Ordering::SeqCst);
        Ok(())
    }
}

/// Runs adapter failure paths with real ArceOS physical page allocation.
pub fn verify_failure_paths() -> Result<(), &'static str> {
    verify_zeroed_backing()?;
    let before = PHYSICAL.released.load(Ordering::SeqCst);
    let domain = Arc::new(FailingDomain::new(
        9001,
        IovaWindow {
            start: 0x1000,
            end: 0x3000,
        },
        true,
    ));
    let dma = IommuDma::with_physical(domain.clone(), &PHYSICAL);
    let two_pages = Layout::from_size_align(2 * PAGE_SIZE, PAGE_SIZE)
        .map_err(|_| "invalid two-page fault test layout")?;
    if !matches!(
        unsafe { dma.try_alloc_contiguous(DmaConstraints::new(u64::MAX), two_pages) },
        Err(DmaError::MappingFailed)
    ) {
        return Err("partial mapping failure did not return MappingFailed");
    }
    if domain.mapped_pages.load(Ordering::SeqCst) != 0
        || domain.unmap_calls.load(Ordering::SeqCst) != 1
        || PHYSICAL.released.load(Ordering::SeqCst) != before + 1
    {
        return Err("partial mapping failure did not synchronize rollback and free pages");
    }
    let one_page = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE)
        .map_err(|_| "invalid one-page fault test layout")?;
    let reused = unsafe { dma.try_alloc_contiguous(DmaConstraints::new(u64::MAX), one_page) }
        .map_err(|_| "rolled-back IOVA could not be reused")?;
    if reused.dma_addr().as_u64() != 0x1000 {
        return Err("partial rollback did not release the IOVA");
    }
    unsafe { dma.try_dealloc_contiguous(reused) }
        .map_err(|_| "reused fault test IOVA release failed")?;
    if PHYSICAL.released.load(Ordering::SeqCst) != before + 2 {
        return Err("reused fault test allocation did not free its pages");
    }

    let domain = Arc::new(FailingDomain::new(
        9002,
        IovaWindow {
            start: 0x1000,
            end: 0x2000,
        },
        false,
    ));
    let dma = IommuDma::with_physical(domain.clone(), &PHYSICAL);
    let held = unsafe { dma.try_alloc_contiguous(DmaConstraints::new(u64::MAX), one_page) }
        .map_err(|_| "failed to create mapping for invalidation fault test")?;
    domain.fail_unmap.store(true, Ordering::SeqCst);
    if !matches!(
        unsafe { dma.try_dealloc_contiguous(held) },
        Err(DmaError::UnmapFailed)
    ) {
        return Err("failed IOTLB invalidation was not reported");
    }
    if PHYSICAL.released.load(Ordering::SeqCst) != before + 2
        || domain.mapped_pages.load(Ordering::SeqCst) != 1
    {
        return Err("failed IOTLB invalidation released reachable physical pages");
    }
    if !matches!(
        unsafe { dma.try_alloc_contiguous(DmaConstraints::new(u64::MAX), one_page) },
        Err(DmaError::NoIova)
    ) {
        return Err("failed IOTLB invalidation released a quarantined IOVA");
    }
    if PHYSICAL.released.load(Ordering::SeqCst) != before + 3 {
        return Err("failed IOVA allocation did not free its new physical pages");
    }
    Ok(())
}

fn verify_zeroed_backing() -> Result<(), &'static str> {
    let domain = Arc::new(FailingDomain::new(
        9003,
        IovaWindow {
            start: 0x1000,
            end: 0x5000,
        },
        false,
    ));
    let dma = IommuDma::with_physical(domain, &DIRTY_PHYSICAL);
    let layout = Layout::from_size_align(17, 1).map_err(|_| "invalid dirty-page layout")?;
    let constraints = DmaConstraints::new(u64::MAX);

    // SAFETY: the test owns the returned handle until synchronized release.
    let contiguous = unsafe { dma.try_alloc_contiguous(constraints, layout) }
        .map_err(|_| "dirty-page contiguous allocation failed")?;
    // SAFETY: the backend requests one complete physical page for this handle.
    if unsafe { core::slice::from_raw_parts(contiguous.as_ptr().as_ptr(), PAGE_SIZE) }
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err("translated contiguous DMA exposed dirty tail bytes");
    }
    // SAFETY: the fake domain has no in-flight device access.
    unsafe { dma.try_dealloc_contiguous(contiguous) }
        .map_err(|_| "dirty-page contiguous release failed")?;

    let mut source = [0x5a; 17];
    // SAFETY: the source remains live and exclusively borrowed until unmap;
    // the fake domain does not issue device accesses.
    let streaming = unsafe {
        dma.map_streaming(
            constraints,
            NonNull::from(&mut source[0]),
            NonZeroUsize::new(source.len()).ok_or("empty dirty-page source")?,
            DmaDirection::ToDevice,
        )
    }
    .map_err(|_| "dirty-page streaming mapping failed")?;
    let bounce = streaming
        .bounce_ptr()
        .ok_or("streaming DMA has no bounce page")?;
    // SAFETY: the mapping owns one complete bounce page until unmap.
    if unsafe { core::slice::from_raw_parts(bounce.as_ptr(), PAGE_SIZE) }
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err("translated streaming DMA exposed dirty tail bytes");
    }
    // SAFETY: the fake domain has no in-flight device access.
    unsafe { dma.try_unmap_streaming(streaming) }
        .map_err(|_| "dirty-page streaming release failed")?;
    Ok(())
}
