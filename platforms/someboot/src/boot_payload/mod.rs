//! Shared initramfs publication and ownership transfer.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(efi)]
use crate::mem::{MemoryDescriptor, MemoryType, add_memory_descriptor};

static PUBLISHED: AtomicBool = AtomicBool::new(false);
static START: AtomicUsize = AtomicUsize::new(0);
static END: AtomicUsize = AtomicUsize::new(0);
static PUBLISHED_END: AtomicUsize = AtomicUsize::new(0);
static RECLAIMABLE: AtomicBool = AtomicBool::new(false);
#[cfg(efi)]
static PENDING_START: AtomicUsize = AtomicUsize::new(0);
#[cfg(efi)]
static PENDING_END: AtomicUsize = AtomicUsize::new(0);

/// Physical host archive pinned until the runtime finishes unpacking it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InitramfsRange {
    pub start: usize,
    pub end: usize,
    pub reclaimable: bool,
}

pub fn initramfs_range() -> Option<InitramfsRange> {
    let end = END.load(Ordering::Acquire);
    (end != 0).then(|| InitramfsRange {
        start: START.load(Ordering::Relaxed),
        end,
        reclaimable: RECLAIMABLE.load(Ordering::Relaxed),
    })
}

/// The unique archive owner. Byte borrows cannot outlive this value or cross
/// `finish`, which consumes ownership before offering pages to an allocator.
#[derive(Debug)]
pub struct Initramfs {
    range: InitramfsRange,
}
impl Initramfs {
    pub fn bytes(&self) -> &[u8] {
        let pointer = crate::mem::phys_to_virt(self.range.start);
        // SAFETY: publication validated and reserved these immutable bytes;
        // this sole owner pins them for the lifetime of the returned borrow.
        unsafe {
            core::slice::from_raw_parts(pointer.cast_const(), self.range.end - self.range.start)
        }
    }
    pub fn finish(self) -> Option<InitramfsReclaim> {
        if !self.range.reclaimable {
            return None;
        }
        let range = owned_pages(self.range)?;
        #[cfg(feature = "boot-resources")]
        let range = {
            let owned_end = OWNED_END.load(Ordering::Acquire);
            let owned = crate::boot_resources::PhysicalRange::new(
                OWNED_START.load(Ordering::Relaxed) as u64,
                owned_end.saturating_sub(OWNED_START.load(Ordering::Relaxed)) as u64,
            );
            if let Some(resource) = crate::boot_resources::retire_transferred(owned) {
                log::info!("host initramfs returned to resource arena");
                let end = range.end()?;
                let start = range.start.max(resource.end()?);
                if start >= end {
                    RETURNED.store(3, Ordering::Release);
                    return None;
                }
                crate::boot_resources::PhysicalRange::new(start, end - start)
            } else {
                range
            }
        };
        OWNED_START.store(range.start as usize, Ordering::Relaxed);
        OWNED_END.store(range.end()? as usize, Ordering::Release);
        Some(InitramfsReclaim { range })
    }
}
/// One-shot transfer into the general allocator. Failure or Drop retains the
/// reservation; only a confirmed allocator insertion changes exported ownership.
#[derive(Debug)]
pub struct InitramfsReclaim {
    range: crate::boot_resources::PhysicalRange,
}
impl InitramfsReclaim {
    pub fn range(&self) -> crate::boot_resources::PhysicalRange {
        self.range
    }
    /// # Safety
    /// The caller must have successfully transferred this complete range to the
    /// allocator. No archive byte borrow or other owner may remain.
    pub unsafe fn complete(self) {
        RETURNED.store(2, Ordering::Release);
    }
}
static RETURNED: AtomicUsize = AtomicUsize::new(0);
static OWNED_START: AtomicUsize = AtomicUsize::new(0);
static OWNED_END: AtomicUsize = AtomicUsize::new(0);

pub fn take_initramfs_range() -> Option<Initramfs> {
    let end = END.swap(0, Ordering::AcqRel);
    (end != 0).then(|| Initramfs {
        range: InitramfsRange {
            start: START.load(Ordering::Relaxed),
            end,
            reclaimable: RECLAIMABLE.load(Ordering::Relaxed),
        },
    })
}
fn owned_pages(range: InitramfsRange) -> Option<crate::boot_resources::PhysicalRange> {
    let explicit_end = OWNED_END.load(Ordering::Acquire);
    let (start, end) = if explicit_end != 0 {
        (
            OWNED_START
                .load(Ordering::Relaxed)
                .checked_next_multiple_of(4096)?,
            explicit_end & !4095,
        )
    } else {
        (
            range.start.checked_next_multiple_of(4096)?,
            range.end & !4095,
        )
    };
    (start < end).then_some(crate::boot_resources::PhysicalRange::new(
        start as u64,
        (end - start) as u64,
    ))
}
#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) fn returned_pages() -> Option<crate::boot_resources::PhysicalRange> {
    if RETURNED.load(Ordering::Acquire) != 2 {
        return None;
    }
    owned_pages(InitramfsRange {
        start: START.load(Ordering::Relaxed),
        end: PUBLISHED_END.load(Ordering::Relaxed),
        reclaimable: true,
    })
}
#[cfg(all(efi, target_arch = "loongarch64"))]
pub(crate) fn publish_inherited(
    start: usize,
    end: usize,
    allocation: crate::boot_resources::PhysicalRange,
) {
    OWNED_START.store(allocation.start as usize, Ordering::Relaxed);
    OWNED_END.store(
        allocation.end().expect("validated archive reservation") as usize,
        Ordering::Release,
    );
    publish(start, end, true);
}

pub(crate) fn publish(start: usize, end: usize, reclaimable: bool) {
    // Only the boot CPU publishes, before secondary CPUs or archive consumers
    // can run. AArch64 exclusive atomics cannot be used before MMU enablement.
    assert!(start < end && !PUBLISHED.load(Ordering::Relaxed));
    PUBLISHED.store(true, Ordering::Relaxed);
    RECLAIMABLE.store(reclaimable, Ordering::Relaxed);
    START.store(start, Ordering::Relaxed);
    PUBLISHED_END.store(end, Ordering::Relaxed);
    END.store(end, Ordering::Release);
}

/// Registers a UEFI loader allocation before ExitBootServices. It remains
/// invisible to the runtime until the final firmware map has reserved it.
#[cfg(efi)]
pub(crate) fn stage_uefi(start: usize, end: usize) {
    assert!(start < end && PENDING_END.load(Ordering::Relaxed) == 0);
    PENDING_START.store(start, Ordering::Relaxed);
    PENDING_END.store(end, Ordering::Release);
}

#[cfg(efi)]
pub(crate) fn reserve_staged_uefi() {
    let end = PENDING_END.load(Ordering::Acquire);
    if end == 0 {
        return;
    }
    let start = PENDING_START.load(Ordering::Relaxed);
    assert!(end.checked_add(crate::consts::PAGE_SIZE - 1).is_some());
    let reservation = MemoryDescriptor::new_aligned(
        start,
        end - start,
        MemoryType::BootData,
        crate::consts::PAGE_SIZE,
    );
    add_memory_descriptor(reservation)
        .unwrap_or_else(|error| panic!("failed to reserve UEFI host initramfs: {error:?}"));
    publish(start, end, true);
}

#[cfg(efi)]
pub(crate) fn staged_uefi() -> Option<(usize, usize)> {
    let end = PENDING_END.load(Ordering::Acquire);
    (end != 0).then(|| (PENDING_START.load(Ordering::Relaxed), end))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn archive_claim_is_unique_and_failed_return_keeps_ownership_reserved() {
        // Only metadata is exercised here; target address translation and real
        // allocator insertion are covered by the EFI smoke, not this host test.
        let storage = alloc::vec![0u8; 4 * 4096];
        let start = storage.as_ptr() as usize;
        publish(start, start + storage.len(), true);
        let archive = take_initramfs_range().unwrap();
        assert!(take_initramfs_range().is_none());
        assert!(initramfs_range().is_none());
        let reclaim = archive.finish().unwrap();
        let pages = reclaim.range();
        assert!(pages.start.is_multiple_of(4096) && pages.len.is_multiple_of(4096));
        assert!(pages.start >= start as u64);
        assert!(pages.end().unwrap() <= (start + storage.len()) as u64);
        drop(reclaim);
        assert_eq!(RETURNED.load(Ordering::Acquire), 0);
        assert!(take_initramfs_range().is_none());
    }
}
