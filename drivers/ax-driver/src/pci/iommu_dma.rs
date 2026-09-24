//! Device-scoped DMA address allocation for a bound PCI IOMMU domain.

use alloc::{collections::BTreeMap, sync::Arc};
use core::{
    alloc::Layout,
    num::{NonZeroU64, NonZeroUsize},
    ptr::NonNull,
};

use ax_sync::SpinLock;
use dma_api::{
    DeviceDma, DmaAddr, DmaAllocHandle, DmaConstraints, DmaDeviceInfo, DmaDirection, DmaDomainId,
    DmaError, DmaMapHandle, DmaOp,
};
use rdif_iommu::{IommuDomain, MapPermissions};

const PAGE_SIZE: usize = 4096;

/// The direct handle owns physical pages. Only the IOMMU adapter may release
/// them, after a successful IOTLB invalidation. The copied handle in the
/// caller-facing DMA object is an address token, not a second owner.
struct OwnedHandle(DmaAllocHandle);

// SAFETY: the allocation remains live while this token is stored. The only
// operation on its pointers is release, after the device has stopped and an
// IOMMU invalidation has completed. The state lock serializes that release.
unsafe impl Send for OwnedHandle {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordKind {
    Pending,
    Releasing,
    Contiguous,
    Coherent,
    Streaming,
    Mmio,
    Quarantined,
}

struct Record {
    len: usize,
    kind: RecordKind,
    backing: Option<OwnedHandle>,
}

/// One allocator is shared by every DMA capability and MSI lease of a PCI
/// function. `records` reserves both active and quarantined IOVA ranges.
pub(crate) struct IommuDma {
    domain: Arc<dyn IommuDomain>,
    physical: &'static dyn DmaOp,
    records: SpinLock<BTreeMap<u64, Record>>,
}

impl IommuDma {
    pub(crate) fn new(domain: Arc<dyn IommuDomain>) -> Self {
        Self::with_physical(domain, axklib::dma::op())
    }

    fn with_physical(domain: Arc<dyn IommuDomain>, physical: &'static dyn DmaOp) -> Self {
        assert_ne!(domain.id().0, 0, "IOMMU domain ID must be nonzero");
        Self {
            domain,
            physical,
            records: SpinLock::new(BTreeMap::new()),
        }
    }

    pub(crate) fn device(self: &Arc<Self>, info: DmaDeviceInfo) -> Result<DeviceDma, DmaError> {
        DeviceDma::new_shared(info, self.clone())
    }

    /// Maps one MSI target page. The returned lease keeps the translation and
    /// its IOVA reservation alive until the PCI interrupt lease is dropped.
    pub(crate) fn map_mmio_page(self: &Arc<Self>, physical: u64) -> Result<MsiMapping, DmaError> {
        let page = physical & !(PAGE_SIZE as u64 - 1);
        let iova = self.reserve(
            PAGE_SIZE,
            DmaConstraints::new(u64::MAX),
            PAGE_SIZE,
            Some(page),
        )?;
        if self
            .domain
            .map_pages(
                iova,
                page,
                PAGE_SIZE,
                MapPermissions::WRITE | MapPermissions::MMIO,
            )
            .is_err()
        {
            // A single-page map must be atomic on failure. The SMMUv3 core
            // checks and allocates its leaf table before publishing the PTE.
            self.records.lock_irqsave().remove(&iova);
            return Err(DmaError::MappingFailed);
        }
        self.finish_reservation(iova, RecordKind::Mmio, None);
        Ok(MsiMapping {
            backend: self.clone(),
            physical_page: page,
            iova,
        })
    }

    fn reserve(
        &self,
        len: usize,
        constraints: DmaConstraints,
        align: usize,
        avoid_physical: Option<u64>,
    ) -> Result<u64, DmaError> {
        let window = self.domain.window();
        let floor = (window.start as u128).max(PAGE_SIZE as u128);
        let ceiling = (window.end as u128).min(constraints.addr_mask as u128 + 1);
        let alignment = align.max(constraints.align).max(PAGE_SIZE) as u128;
        let mut records = self.records.lock_irqsave();
        let mut cursor = floor;

        let mut available = None;
        for (&occupied_start, occupied) in records.iter() {
            let gap_end = (occupied_start as u128).min(ceiling);
            available = find_in_gap(
                cursor,
                gap_end,
                len,
                alignment,
                constraints.boundary,
                avoid_physical,
            );
            if available.is_some() {
                break;
            }
            cursor = cursor.max(occupied_start as u128 + occupied.len as u128);
            if cursor >= ceiling {
                return Err(DmaError::NoIova);
            }
        }

        let iova = available
            .or_else(|| {
                find_in_gap(
                    cursor,
                    ceiling,
                    len,
                    alignment,
                    constraints.boundary,
                    avoid_physical,
                )
            })
            .ok_or(DmaError::NoIova)?;
        records.insert(
            iova,
            Record {
                len,
                kind: RecordKind::Pending,
                backing: None,
            },
        );
        Ok(iova)
    }

    fn finish_reservation(&self, iova: u64, kind: RecordKind, backing: Option<OwnedHandle>) {
        let mut records = self.records.lock_irqsave();
        let record = records
            .get_mut(&iova)
            .expect("IOVA reservation must remain live until publication");
        debug_assert_eq!(record.kind, RecordKind::Pending);
        record.kind = kind;
        record.backing = backing;
    }

    fn map_backing(
        &self,
        direct: DmaAllocHandle,
        requested_layout: Layout,
        constraints: DmaConstraints,
        kind: RecordKind,
        permissions: MapPermissions,
    ) -> Result<DmaAllocHandle, DmaError> {
        let mapped_len = direct.size();
        // The IOMMU exposes complete pages, including bytes beyond the
        // caller's requested length. Clear the full backing allocation before
        // any PTE is published so a device cannot read a previous owner's
        // data through the tail or a Stage 1 write mapping.
        // SAFETY: `direct` owns writable memory for its complete allocation
        // layout, and it has not been published to a device yet.
        unsafe { direct.as_ptr().write_bytes(0, mapped_len) };
        if kind != RecordKind::Coherent {
            self.physical.flush(direct.as_ptr(), mapped_len);
        }
        let physical = direct.dma_addr().as_u64();
        let iova = match self.reserve(
            mapped_len,
            constraints,
            requested_layout.align(),
            Some(physical),
        ) {
            Ok(iova) => iova,
            Err(error) => {
                self.release_direct(direct, kind)?;
                return Err(error);
            }
        };

        let mut mapped = 0usize;
        while mapped < mapped_len {
            let offset = mapped as u64;
            if self
                .domain
                .map_pages(iova + offset, physical + offset, PAGE_SIZE, permissions)
                .is_err()
            {
                let rollback = if mapped == 0 {
                    Ok(())
                } else {
                    self.domain.unmap_and_sync(iova, mapped)
                };
                if rollback.is_err() {
                    // The device may retain a stale translation. Keep the
                    // physical allocation and IOVA, including on this error.
                    self.finish_reservation(
                        iova,
                        RecordKind::Quarantined,
                        Some(OwnedHandle(direct)),
                    );
                    return Err(DmaError::UnmapFailed);
                }
                self.records.lock_irqsave().remove(&iova);
                self.release_direct(direct, kind)?;
                return Err(DmaError::MappingFailed);
            }
            mapped += PAGE_SIZE;
        }

        self.finish_reservation(iova, kind, Some(OwnedHandle(direct)));
        // SAFETY: the direct handle owns the live allocation and the complete
        // IOVA translation is visible. The adapter's record keeps the direct
        // handle until invalidation precedes physical release.
        Ok(unsafe {
            DmaAllocHandle::new(
                direct.as_ptr(),
                direct.allocation_ptr(),
                DmaAddr::from(iova),
                requested_layout,
            )
        })
    }

    fn release_mapping(&self, iova: u64, expected: RecordKind) -> Result<(), DmaError> {
        let len = {
            let mut records = self.records.lock_irqsave();
            let record = records.get_mut(&iova).ok_or(DmaError::UnmapFailed)?;
            if record.kind != expected {
                return Err(DmaError::UnmapFailed);
            }
            record.kind = RecordKind::Releasing;
            record.len
        };

        // A failed sync leaves this reservation and its physical backing in
        // place. No allocator may reuse either resource afterward.
        if self.domain.unmap_and_sync(iova, len).is_err() {
            self.records
                .lock_irqsave()
                .get_mut(&iova)
                .expect("failed IOVA release must retain its reservation")
                .kind = RecordKind::Quarantined;
            return Err(DmaError::UnmapFailed);
        }

        let record = self
            .records
            .lock_irqsave()
            .remove(&iova)
            .expect("mapped IOVA must remain reserved until release");
        if let Some(direct) = record.backing {
            self.release_direct(direct.0, expected)?;
        }
        Ok(())
    }

    fn release_direct(&self, handle: DmaAllocHandle, kind: RecordKind) -> Result<(), DmaError> {
        match kind {
            RecordKind::Coherent => unsafe { self.physical.dealloc_coherent(handle) },
            RecordKind::Contiguous | RecordKind::Streaming => unsafe {
                self.physical.try_dealloc_contiguous(handle)
            },
            _ => Ok(()),
        }
    }
}

impl DmaOp for IommuDma {
    fn page_size(&self) -> usize {
        PAGE_SIZE
    }

    fn domain_id(&self) -> DmaDomainId {
        DmaDomainId::Translated(NonZeroU64::new(self.domain.id().0).expect("valid domain ID"))
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { self.try_alloc_contiguous(constraints, layout) }.ok()
    }

    unsafe fn try_alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        self.allocate(constraints, layout, RecordKind::Contiguous)
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        if let Err(error) = unsafe { self.try_dealloc_contiguous(handle) } {
            log::error!("IOMMU DMA release failed; backing retained: {error}");
        }
    }

    unsafe fn try_dealloc_contiguous(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        self.release_mapping(handle.dma_addr().as_u64(), RecordKind::Contiguous)
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { self.try_alloc_coherent(constraints, layout) }.ok()
    }

    unsafe fn try_alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
    ) -> Result<DmaAllocHandle, DmaError> {
        self.allocate(constraints, layout, RecordKind::Coherent)
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) -> Result<(), DmaError> {
        self.release_mapping(handle.dma_addr().as_u64(), RecordKind::Coherent)
    }

    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: NonNull<u8>,
        size: NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        check_segment_size(size.get(), constraints)?;
        let layout = Layout::from_size_align(size.get(), constraints.align.max(1))?;
        let physical_layout = page_layout(layout)?;
        // Always map adapter-owned bounce pages. The borrowed source may be
        // released even when an IOTLB invalidation fails on unmap.
        let direct = unsafe {
            self.physical
                .try_alloc_contiguous(physical_constraints(physical_layout), physical_layout)
        }?;
        let permissions = match direction {
            DmaDirection::ToDevice => MapPermissions::READ,
            DmaDirection::FromDevice => MapPermissions::WRITE,
            DmaDirection::Bidirectional => MapPermissions::READ | MapPermissions::WRITE,
        };
        let outer = self.map_backing(
            direct,
            layout,
            constraints,
            RecordKind::Streaming,
            permissions,
        )?;
        // SAFETY: `addr` remains caller-owned for the mapping lifetime by the
        // DmaOp contract. `direct` remains live in the adapter's record until
        // IOTLB invalidation, and is the separate device-visible bounce page.
        Ok(unsafe { DmaMapHandle::new(addr, outer.dma_addr(), layout, Some(direct.as_ptr())) })
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        if let Err(error) = unsafe { self.try_unmap_streaming(handle) } {
            log::error!("IOMMU streaming unmap failed; bounce pages retained: {error}");
        }
    }

    unsafe fn try_unmap_streaming(&self, handle: DmaMapHandle) -> Result<(), DmaError> {
        self.release_mapping(handle.dma_addr().as_u64(), RecordKind::Streaming)
    }

    fn flush(&self, addr: NonNull<u8>, size: usize) {
        self.physical.flush(addr, size);
    }

    fn invalidate(&self, addr: NonNull<u8>, size: usize) {
        self.physical.invalidate(addr, size);
    }

    fn flush_invalidate(&self, addr: NonNull<u8>, size: usize) {
        self.physical.flush_invalidate(addr, size);
    }
}

impl IommuDma {
    fn allocate(
        &self,
        constraints: DmaConstraints,
        layout: Layout,
        kind: RecordKind,
    ) -> Result<DmaAllocHandle, DmaError> {
        check_segment_size(layout.size(), constraints)?;
        let physical_layout = page_layout(layout)?;
        let direct = match kind {
            RecordKind::Contiguous => unsafe {
                self.physical
                    .try_alloc_contiguous(physical_constraints(physical_layout), physical_layout)
            }?,
            RecordKind::Coherent => unsafe {
                self.physical
                    .try_alloc_coherent(physical_constraints(physical_layout), physical_layout)
            }?,
            _ => unreachable!(),
        };
        self.map_backing(
            direct,
            layout,
            constraints,
            kind,
            MapPermissions::READ | MapPermissions::WRITE,
        )
    }
}

/// The MSI message address is only valid while this lease exists.
pub(crate) struct MsiMapping {
    backend: Arc<IommuDma>,
    physical_page: u64,
    iova: u64,
}

impl MsiMapping {
    pub(crate) fn dma_address(&self, physical_address: u64) -> u64 {
        assert_eq!(
            physical_address & !(PAGE_SIZE as u64 - 1),
            self.physical_page,
            "MSI target changed after mapping"
        );
        self.iova + (physical_address & (PAGE_SIZE as u64 - 1))
    }
}

impl Drop for MsiMapping {
    fn drop(&mut self) {
        if let Err(error) = self.backend.release_mapping(self.iova, RecordKind::Mmio) {
            log::error!("MSI IOMMU unmap failed; IOVA retained: {error}");
        }
    }
}

fn page_layout(layout: Layout) -> Result<Layout, DmaError> {
    let rounded = layout
        .size()
        .checked_add(PAGE_SIZE - 1)
        .map(|size| size & !(PAGE_SIZE - 1))
        .ok_or(DmaError::NoMemory)?;
    if rounded == 0 {
        return Err(DmaError::ZeroSizedBuffer);
    }
    Ok(Layout::from_size_align(
        rounded,
        layout.align().max(PAGE_SIZE),
    )?)
}

fn physical_constraints(layout: Layout) -> DmaConstraints {
    DmaConstraints::new(u64::MAX).with_align(layout.align())
}

fn check_segment_size(size: usize, constraints: DmaConstraints) -> Result<(), DmaError> {
    if let Some(max) = constraints.max_segment_size
        && size > max
    {
        return Err(DmaError::SegmentTooLarge { size, max });
    }
    Ok(())
}

fn align_up(value: u128, align: u128) -> u128 {
    value.div_ceil(align) * align
}

fn find_in_gap(
    start: u128,
    end: u128,
    len: usize,
    align: u128,
    boundary: Option<usize>,
    avoid_physical: Option<u64>,
) -> Option<u64> {
    if boundary.is_some_and(|boundary| len > boundary) {
        return None;
    }
    let mut candidate = align_up(start, align);
    let len = len as u128;
    loop {
        if candidate + len > end || candidate > u64::MAX as u128 {
            return None;
        }
        if Some(candidate as u64) == avoid_physical {
            candidate += align;
            continue;
        }
        if let Some(boundary) = boundary {
            let boundary = boundary as u128;
            if boundary == 0 || candidate / boundary != (candidate + len - 1) / boundary {
                if boundary == 0 {
                    return None;
                }
                candidate = align_up((candidate / boundary + 1) * boundary, align);
                continue;
            }
        }
        return Some(candidate as u64);
    }
}

#[cfg(feature = "iommu-dma-test")]
mod fault_test;
#[cfg(feature = "iommu-dma-test")]
pub use fault_test::verify_failure_paths;
