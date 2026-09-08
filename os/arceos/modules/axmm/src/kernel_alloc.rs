//! Typed ownership for virtually contiguous kernel allocations.

use core::sync::atomic::{AtomicBool, Ordering};

use ax_alloc::UsageKind;
use ax_hal::{
    cache::TlbShootdownError,
    paging::{MappingFlags, PagingError},
};
use ax_memory_addr::{PAGE_SIZE_4K, VirtAddr, VirtAddrRange};

use crate::{
    MmError, MmResult,
    backend::{KernelVirtualAllocationBackend, KernelVirtualAllocationId},
    kernel_aspace,
};

/// Validated layout of a virtually contiguous kernel allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelVirtualAllocationLayout {
    usable_size: usize,
    leading_guard_pages: usize,
    alignment: usize,
    flags: MappingFlags,
    usage: UsageKind,
}

impl KernelVirtualAllocationLayout {
    /// Creates a page-aligned layout with no guard pages.
    pub fn new(usable_size: usize, flags: MappingFlags, usage: UsageKind) -> MmResult<Self> {
        if usable_size == 0 || !usable_size.is_multiple_of(PAGE_SIZE_4K) {
            return Err(MmError::InvalidInput(
                "kernel virtual allocation size is not page aligned",
            ));
        }
        if flags.is_empty() || flags.contains(MappingFlags::USER) {
            return Err(MmError::InvalidInput(
                "kernel virtual allocation permissions are invalid",
            ));
        }
        Ok(Self {
            usable_size,
            leading_guard_pages: 0,
            alignment: PAGE_SIZE_4K,
            flags,
            usage,
        })
    }

    /// Reserves `pages` unmapped pages immediately before the usable range.
    pub fn with_leading_guard_pages(mut self, pages: usize) -> MmResult<Self> {
        pages
            .checked_mul(PAGE_SIZE_4K)
            .and_then(|guard_size| guard_size.checked_add(self.usable_size))
            .ok_or(MmError::InvalidInput(
                "kernel virtual allocation layout overflows",
            ))?;
        self.leading_guard_pages = pages;
        self.validate_alignment(self.alignment)?;
        Ok(self)
    }

    /// Aligns the reservation and usable range. Both the usable size and
    /// leading guard size must be multiples of this power-of-two alignment.
    pub fn with_alignment(mut self, alignment: usize) -> MmResult<Self> {
        self.validate_alignment(alignment)?;
        self.alignment = alignment;
        Ok(self)
    }

    fn validate_alignment(self, alignment: usize) -> MmResult {
        if alignment < PAGE_SIZE_4K
            || !alignment.is_power_of_two()
            || !self.usable_size.is_multiple_of(alignment)
            || !self
                .leading_guard_pages
                .is_multiple_of(alignment / PAGE_SIZE_4K)
        {
            return Err(MmError::InvalidInput(
                "kernel virtual allocation alignment is invalid",
            ));
        }
        Ok(())
    }

    fn total_size(self) -> MmResult<usize> {
        self.leading_guard_pages
            .checked_mul(PAGE_SIZE_4K)
            .and_then(|guard_size| guard_size.checked_add(self.usable_size))
            .ok_or(MmError::InvalidInput(
                "kernel virtual allocation layout overflows",
            ))
    }
}

/// Failure while retiring a kernel virtual allocation.
#[derive(Debug, Clone, Copy, Eq, PartialEq, thiserror::Error)]
pub enum KernelVirtualReleaseError {
    /// Another sleepable worker owns the global retire pass.
    #[error("kernel virtual allocation retire pass is busy")]
    Busy,
    /// The mapping metadata or page table could not complete its transition.
    #[error(transparent)]
    Mapping(#[from] MmError),
    /// At least one CPU did not acknowledge the invalidated range.
    #[error(transparent)]
    Tlb(#[from] TlbShootdownError),
}

static KERNEL_VIRTUAL_RETIRE_ACTIVE: AtomicBool = AtomicBool::new(false);

struct KernelVirtualRetireLease;

impl KernelVirtualRetireLease {
    fn try_acquire() -> Option<Self> {
        KERNEL_VIRTUAL_RETIRE_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for KernelVirtualRetireLease {
    fn drop(&mut self) {
        KERNEL_VIRTUAL_RETIRE_ACTIVE.store(false, Ordering::Release);
    }
}

/// Unique owner of one virtually contiguous, page-backed kernel range.
///
/// Backing frames are allocated one page at a time, so physical fragmentation
/// cannot turn an otherwise satisfiable allocation into a high-order failure.
/// Dropping the token only publishes `Live -> Retiring`; it never performs a
/// fallible page-table or TLB operation. A sleepable reclaimer makes every leaf
/// non-present, synchronously invalidates the range on all CPUs, and only then
/// removes the metadata owner whose final reference releases the frames.
#[derive(Debug)]
pub struct KernelVirtualAllocation {
    id: KernelVirtualAllocationId,
    reservation: VirtAddrRange,
    usable: VirtAddrRange,
    active: bool,
}

impl KernelVirtualAllocation {
    /// Allocates and publishes a new kernel virtual range.
    pub fn allocate(layout: KernelVirtualAllocationLayout) -> MmResult<Self> {
        let total_size = layout.total_size()?;
        let guard_size =
            layout
                .leading_guard_pages
                .checked_mul(PAGE_SIZE_4K)
                .ok_or(MmError::InvalidInput(
                    "kernel virtual allocation guard overflows",
                ))?;
        // All data frames and their Arc/Vec owner are prepared before taking
        // the kernel address-space lock. Allocation pressure can therefore
        // reclaim caches without reentering that lock.
        let allocation = KernelVirtualAllocationBackend::allocate(
            layout.usage,
            layout.leading_guard_pages,
            layout.usable_size / PAGE_SIZE_4K,
        )
        .ok_or(MmError::NoMemory)?;
        let reservation_start = {
            let mut aspace = kernel_aspace().lock();
            let limit = VirtAddrRange::new(aspace.base(), aspace.end());
            let start = aspace
                .find_free_area_aligned(aspace.base(), total_size, limit, layout.alignment)
                .ok_or(MmError::NoMemory)?;
            aspace.reserve_kernel_virtual_allocation(
                start,
                total_size,
                layout.flags,
                &allocation,
            )?;
            start
        };
        let usable_start = reservation_start + guard_size;
        let token = Self {
            id: allocation.id(),
            reservation: VirtAddrRange::from_start_size(reservation_start, total_size),
            usable: VirtAddrRange::from_start_size(usable_start, layout.usable_size),
            active: true,
        };
        // From this point every error queues the reserved owner for retirement,
        // including a partially installed prefix. No mapped frame can escape
        // through the local builder's Drop.
        for index in 0..allocation.frame_count() {
            let frame = allocation.expected_frame(index).ok_or(MmError::BadState(
                "kernel virtual allocation lost a reserved frame",
            ))?;
            install_kernel_virtual_page(usable_start + index * PAGE_SIZE_4K, frame, layout.flags)?;
        }
        Ok(token)
    }

    /// Returns the usable range, excluding guard pages.
    pub const fn usable_range(&self) -> VirtAddrRange {
        self.usable
    }

    /// Returns the full reserved range, including guard pages.
    pub const fn reservation_range(&self) -> VirtAddrRange {
        self.reservation
    }

    fn release_inner(&mut self) -> Result<(), KernelVirtualReleaseError> {
        if !self.active {
            return Ok(());
        }
        {
            let mut aspace = kernel_aspace().lock();
            aspace.mark_kernel_virtual_retiring(
                self.id,
                self.reservation.start,
                self.reservation.size(),
            )?;

            // The address-space metadata now owns the frame set. The token can
            // disappear after any later error without exposing or freeing it.
            self.active = false;
        }

        let _retire_lease =
            KernelVirtualRetireLease::try_acquire().ok_or(KernelVirtualReleaseError::Busy)?;
        let mapped = kernel_aspace().lock().prepare_kernel_virtual_release(
            self.id,
            self.reservation.start,
            self.reservation.size(),
        )?;

        ax_hal::cache::flush_tlb_range_all_cpus(mapped.start, mapped.size())?;
        let retired = {
            kernel_aspace().lock().retire_kernel_virtual_allocation(
                self.id,
                self.reservation.start,
                self.reservation.size(),
            )?
        };
        drop(retired);
        Ok(())
    }

    /// Explicitly releases the mapping and reports any quarantine condition.
    pub fn release(mut self) -> Result<(), KernelVirtualReleaseError> {
        self.release_inner()
    }
}

fn install_kernel_virtual_page(
    address: VirtAddr,
    frame: ax_memory_addr::PhysAddr,
    flags: MappingFlags,
) -> MmResult {
    // Concurrent disjoint allocations may fill a shared directory after the
    // snapshot. Each retry prepares a new suffix outside the lock, and a
    // bounded failure leaves the caller's retire token owning the full range.
    for _ in 0..8 {
        let plan = kernel_aspace()
            .lock()
            .page_table_mut()
            .plan_map_page(address, PAGE_SIZE_4K)
            .map_err(kernel_virtual_paging_error)?;
        let deposit = plan
            .prepare(frame, flags)
            .map_err(kernel_virtual_paging_error)?;
        let applied = {
            kernel_aspace()
                .lock()
                .page_table_mut()
                .try_map_page_with(deposit)
        };
        match applied {
            Ok(()) => return Ok(()),
            Err(error) => {
                let stale = matches!(error.error(), PagingError::StaleMapDeposit { .. });
                let cause = kernel_virtual_paging_error(error.error().clone());
                // Failed deposits own unpublished page-table pages. Release
                // those pages only after the page-table guard has gone away.
                drop(error);
                if !stale {
                    return Err(cause);
                }
            }
        }
    }
    Err(MmError::BadState(
        "kernel virtual page-table plan remained contended",
    ))
}

fn kernel_virtual_paging_error(error: PagingError) -> MmError {
    match error {
        PagingError::NoMemory => MmError::NoMemory,
        PagingError::MappingConflict { .. } => MmError::AlreadyExists,
        PagingError::NotMapped => MmError::BadAddress,
        _ => MmError::BadState("kernel virtual page-table operation failed"),
    }
}

impl Drop for KernelVirtualAllocation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let result = kernel_aspace().lock().mark_kernel_virtual_retiring(
            self.id,
            self.reservation.start,
            self.reservation.size(),
        );
        if result.is_ok() {
            self.active = false;
        } else if let Err(error) = result {
            error!(
                "kernel virtual allocation {:?} [{:#x}..{:#x}) could not enter the retire queue: \
                 {error}",
                self.id,
                self.reservation.start.as_usize(),
                self.reservation.end.as_usize(),
            );
        }
    }
}

/// Result of one bounded kernel virtual allocation retire pass.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct KernelVirtualQuarantineRetry {
    attempted: usize,
    reclaimed: usize,
    first_error: Option<KernelVirtualReleaseError>,
}

impl KernelVirtualQuarantineRetry {
    /// Number of retire candidates visited during this pass.
    pub const fn attempted(self) -> usize {
        self.attempted
    }

    /// Number of candidates fully retired after TLB acknowledgement.
    pub const fn reclaimed(self) -> usize {
        self.reclaimed
    }

    /// Number of candidates that remain queued for a later retry.
    pub const fn failed(self) -> usize {
        self.attempted - self.reclaimed
    }

    /// First structured failure observed while the pass continued scanning.
    pub const fn first_error(self) -> Option<KernelVirtualReleaseError> {
        self.first_error
    }
}

type KernelVirtualRetireCandidate = (KernelVirtualAllocationId, VirtAddr, usize);

fn retry_kernel_virtual_quarantines_with(
    limit: usize,
    mut next: impl FnMut(Option<VirtAddr>) -> Option<KernelVirtualRetireCandidate>,
    mut retry: impl FnMut(KernelVirtualRetireCandidate) -> Result<(), KernelVirtualReleaseError>,
) -> KernelVirtualQuarantineRetry {
    let mut report = KernelVirtualQuarantineRetry {
        attempted: 0,
        reclaimed: 0,
        first_error: None,
    };
    let mut cursor = None;
    while report.attempted < limit {
        let Some(candidate @ (_, start, _)) = next(cursor) else {
            break;
        };
        cursor = Some(start);
        report.attempted += 1;
        match retry(candidate) {
            Ok(()) => report.reclaimed += 1,
            Err(error) => {
                report.first_error.get_or_insert(error);
            }
        }
    }
    report
}

fn retry_kernel_virtual_quarantine(
    (id, start, size): KernelVirtualRetireCandidate,
) -> Result<(), KernelVirtualReleaseError> {
    let mapped = kernel_aspace()
        .lock()
        .prepare_kernel_virtual_release(id, start, size)?;
    ax_hal::cache::flush_tlb_range_all_cpus(mapped.start, mapped.size())?;
    let retired = {
        kernel_aspace()
            .lock()
            .retire_kernel_virtual_allocation(id, start, size)?
    };
    drop(retired);
    Ok(())
}

/// Retries a bounded number of retiring or quarantined kernel virtual ranges.
///
/// Each candidate is attempted at most once per pass. A broken or timed-out
/// range therefore cannot starve later independent ranges; failures remain
/// published in their current state for the next pass.
pub fn retry_kernel_virtual_quarantines(limit: usize) -> KernelVirtualQuarantineRetry {
    let Some(_retire_lease) = KernelVirtualRetireLease::try_acquire() else {
        return KernelVirtualQuarantineRetry {
            attempted: 0,
            reclaimed: 0,
            first_error: None,
        };
    };
    retry_kernel_virtual_quarantines_with(
        limit,
        |cursor| {
            kernel_aspace()
                .lock()
                .next_kernel_virtual_retire_after(cursor)
        },
        retry_kernel_virtual_quarantine,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_rejects_empty_unaligned_and_user_ranges() {
        let kernel_flags = MappingFlags::READ | MappingFlags::WRITE;
        assert!(matches!(
            KernelVirtualAllocationLayout::new(0, kernel_flags, UsageKind::TaskStack),
            Err(MmError::InvalidInput(_))
        ));
        assert!(matches!(
            KernelVirtualAllocationLayout::new(
                PAGE_SIZE_4K + 1,
                kernel_flags,
                UsageKind::TaskStack
            ),
            Err(MmError::InvalidInput(_))
        ));
        assert!(matches!(
            KernelVirtualAllocationLayout::new(
                PAGE_SIZE_4K,
                MappingFlags::READ | MappingFlags::USER,
                UsageKind::TaskStack
            ),
            Err(MmError::InvalidInput(_))
        ));
    }

    #[test]
    fn guard_pages_extend_reservation_without_reducing_usable_size() {
        let layout = KernelVirtualAllocationLayout::new(
            64 * PAGE_SIZE_4K,
            MappingFlags::READ | MappingFlags::WRITE,
            UsageKind::TaskStack,
        )
        .unwrap()
        .with_leading_guard_pages(1)
        .unwrap();
        assert_eq!(layout.usable_size, 64 * PAGE_SIZE_4K);
        assert_eq!(layout.total_size().unwrap(), 65 * PAGE_SIZE_4K);
    }

    #[test]
    fn quarantine_retry_continues_after_an_independent_failure() {
        let candidates = [
            (
                KernelVirtualAllocationId::for_test(1),
                VirtAddr::from(0x1000),
                PAGE_SIZE_4K,
            ),
            (
                KernelVirtualAllocationId::for_test(2),
                VirtAddr::from(0x2000),
                PAGE_SIZE_4K,
            ),
            (
                KernelVirtualAllocationId::for_test(3),
                VirtAddr::from(0x3000),
                PAGE_SIZE_4K,
            ),
        ];

        let report = retry_kernel_virtual_quarantines_with(
            usize::MAX,
            |cursor| {
                candidates
                    .iter()
                    .copied()
                    .find(|(_, start, _)| cursor.is_none_or(|previous| *start > previous))
            },
            |(_, start, _)| {
                if start == VirtAddr::from(0x1000) {
                    Err(KernelVirtualReleaseError::Tlb(TlbShootdownError::Timeout))
                } else {
                    Ok(())
                }
            },
        );

        assert_eq!(report.attempted(), 3);
        assert_eq!(report.reclaimed(), 2);
        assert_eq!(report.failed(), 1);
        assert_eq!(
            report.first_error(),
            Some(KernelVirtualReleaseError::Tlb(TlbShootdownError::Timeout))
        );
    }

    #[test]
    fn quarantine_retry_limit_counts_attempts_instead_of_successes() {
        let candidates = [
            (
                KernelVirtualAllocationId::for_test(4),
                VirtAddr::from(0x4000),
                PAGE_SIZE_4K,
            ),
            (
                KernelVirtualAllocationId::for_test(5),
                VirtAddr::from(0x5000),
                PAGE_SIZE_4K,
            ),
            (
                KernelVirtualAllocationId::for_test(6),
                VirtAddr::from(0x6000),
                PAGE_SIZE_4K,
            ),
        ];

        let report = retry_kernel_virtual_quarantines_with(
            2,
            |cursor| {
                candidates
                    .iter()
                    .copied()
                    .find(|(_, start, _)| cursor.is_none_or(|previous| *start > previous))
            },
            |(_, start, _)| {
                (start != VirtAddr::from(0x4000)).then_some(()).ok_or(
                    KernelVirtualReleaseError::Tlb(TlbShootdownError::Unsupported),
                )
            },
        );

        assert_eq!(report.attempted(), 2);
        assert_eq!(report.reclaimed(), 1);
        assert_eq!(report.failed(), 1);
    }
}
