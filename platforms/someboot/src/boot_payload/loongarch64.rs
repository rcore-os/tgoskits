//! LoongArch consumption of the architecture-neutral handoff record.
use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(crate) use crate::handoff::protocol::HANDOFF_GUID;
use crate::{
    handoff::protocol::HandoffRecord,
    mem::{KIMAGE_MAP_ALIGN, MemoryDescriptor, MemoryType, add_memory_descriptor},
};
pub(crate) const RETIRED_HANDOFF_GUID: uefi::Guid =
    uefi::guid!("5d34b5d2-9a5b-4d0f-8e3c-6bb3dfc6a1fe");
static INHERITED_PAYLOAD: AtomicUsize = AtomicUsize::new(0);
struct SnapshotStorage(UnsafeCell<HandoffRecord>);
// SAFETY: the EFI boot CPU initializes this once, before SMP or OS entry. It is
// immutable afterward, independently of the producer's reusable firmware table.
unsafe impl Sync for SnapshotStorage {}
static SNAPSHOT: SnapshotStorage = SnapshotStorage(UnsafeCell::new(HandoffRecord::empty()));

/// # Safety
/// Early boot only, before consumers or SMP. The trusted firmware allocation
/// must stay live until its memory map and resources have been restored.
pub(crate) unsafe fn remember_inherited_payload(pointer: *const HandoffRecord) {
    // SAFETY: the EFI configuration entry supplies the retained allocation.
    let payload =
        unsafe { HandoffRecord::from_ptr(pointer) }.expect("invalid inherited EFI handoff");
    assert!(payload.boot_services_exited(), "expected post-EBS handoff");
    for resource in payload.resource_slice().expect("validated resource count") {
        assert!(
            resource.range.end().is_some_and(|end| end <= 1 << 48),
            "invalid LoongArch physical resource"
        );
    }
    // SAFETY: boot CPU only, and this destination is never written again.
    unsafe {
        SNAPSHOT.0.get().write(*payload);
        crate::handoff::received::remember(payload.metadata());
    }
    INHERITED_PAYLOAD.store(pointer as usize, Ordering::Release);
}

pub(crate) fn inherited_table() -> Option<usize> {
    let address = INHERITED_PAYLOAD.load(Ordering::Acquire);
    (address != 0).then_some(address)
}

pub(crate) fn inherited_payload() -> Option<&'static HandoffRecord> {
    inherited_table()?;
    // SAFETY: release/acquire publishes the immutable per-image copy, which is
    // separate from the reusable configuration table modified by producers.
    Some(unsafe { &*SNAPSHOT.0.get() })
}

pub(crate) fn restore_inherited_memory_map() {
    let Some(payload) = inherited_payload() else {
        return;
    };
    payload.validate().expect("invalid inherited EFI handoff");
    let start = usize::try_from(payload.image.start).expect("target address overflows");
    let end = usize::try_from(payload.image.end().expect("validated image end"))
        .expect("target end overflows")
        .checked_add(KIMAGE_MAP_ALIGN - 1)
        .expect("target alignment overflows")
        / KIMAGE_MAP_ALIGN
        * KIMAGE_MAP_ALIGN;
    assert!(
        payload
            .resource_slice()
            .unwrap()
            .iter()
            .any(
                |r| r.range.contains(crate::boot_resources::PhysicalRange::new(
                    start as u64,
                    (end - start) as u64
                ))
            ),
        "aligned target exceeds resource reservation"
    );
    for region in &payload.memory_map[..payload.memory_map_len as usize] {
        let region_start = usize::try_from(region.range.start).expect("memory address overflows");
        let region_end =
            usize::try_from(region.range.end().unwrap()).expect("memory end overflows");
        let kind = match region.kind {
            0 => MemoryType::Free,
            1 => MemoryType::Ram,
            2 => MemoryType::KImage,
            3 => MemoryType::Reserved,
            4 => MemoryType::Mmio,
            5 => MemoryType::PerCpuData,
            _ => unreachable!("validated memory type"),
        };
        let ranges = if region_start < end && region_end > start {
            [
                (region_start, region_end.min(start)),
                (region_start.max(end), region_end),
            ]
        } else {
            [(region_start, region_end), (0, 0)]
        };
        for (a, b) in ranges {
            if a < b {
                add_memory_descriptor(MemoryDescriptor::new_with_range(a..b, kind))
                    .unwrap_or_else(|error| panic!("failed to restore inherited map: {error:?}"));
            }
        }
    }
}
