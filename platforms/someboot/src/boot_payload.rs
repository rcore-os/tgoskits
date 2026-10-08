use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[cfg(efi)]
use crate::mem::{MemoryDescriptor, MemoryType, add_memory_descriptor};

static PUBLISHED: AtomicBool = AtomicBool::new(false);
static START: AtomicUsize = AtomicUsize::new(0);
static END: AtomicUsize = AtomicUsize::new(0);
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

/// Transfers the reserved archive to its sole runtime consumer.
///
/// The caller must finish every archive borrow before reclaiming owned pages.
/// Subsequent callers and metadata queries cannot observe the consumed range.
pub fn take_initramfs_range() -> Option<InitramfsRange> {
    let end = END.swap(0, Ordering::AcqRel);
    (end != 0).then(|| InitramfsRange {
        start: START.load(Ordering::Relaxed),
        end,
        reclaimable: RECLAIMABLE.load(Ordering::Relaxed),
    })
}

pub(crate) fn publish(start: usize, end: usize, reclaimable: bool) {
    // Only the boot CPU publishes, before secondary CPUs or archive consumers
    // can run. AArch64 exclusive atomics cannot be used before MMU enablement.
    assert!(start < end && !PUBLISHED.load(Ordering::Relaxed));
    PUBLISHED.store(true, Ordering::Relaxed);
    RECLAIMABLE.store(reclaimable, Ordering::Relaxed);
    START.store(start, Ordering::Relaxed);
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
        MemoryType::Reserved,
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
