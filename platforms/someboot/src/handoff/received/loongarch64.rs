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

fn receiver_map(payload: &HandoffRecord) -> crate::handoff::memory::BootMap {
    #[cfg(feature = "boot-resources")]
    let retained = crate::boot_resources::retained_resources(payload.resource_slice().unwrap())
        .expect("incompatible receiver resources");
    #[cfg(not(feature = "boot-resources"))]
    let retained = [];
    payload
        .receiver_map(&retained)
        .expect("receiver memory map exceeds validated capacity")
}

pub(crate) fn restore_inherited_memory_map() {
    let Some(payload) = inherited_payload() else {
        return;
    };
    let map = receiver_map(payload);
    let start = payload.image.start as usize;
    let end = (payload.image.end().unwrap() as usize)
        .checked_next_multiple_of(KIMAGE_MAP_ALIGN)
        .expect("target alignment overflows");
    for region in &map.regions[..map.len] {
        let a = region.range.start as usize;
        let b = region.range.end().unwrap() as usize;
        // The common early initializer inserts the target's actual KImage span.
        let ranges = if a < end && b > start {
            [(a, b.min(start)), (a.max(end), b)]
        } else {
            [(a, b), (0, 0)]
        };
        for (lo, hi) in ranges {
            if lo < hi {
                add_memory_descriptor(MemoryDescriptor::new_with_range(
                    lo..hi,
                    decode_kind(region.kind),
                ))
                .expect("failed to restore inherited map");
            }
        }
    }
}

pub(crate) fn encode_kind(kind: MemoryType) -> u32 {
    match kind {
        MemoryType::Free => 0,
        MemoryType::Ram => 1,
        MemoryType::KImage => 2,
        MemoryType::Reserved => 3,
        MemoryType::Mmio => 4,
        MemoryType::PerCpuData => 5,
        MemoryType::BootData => 6,
        MemoryType::BootReclaimable => 7,
    }
}
fn decode_kind(kind: u32) -> MemoryType {
    match kind {
        0 => MemoryType::Free,
        1 => MemoryType::Ram,
        2 => MemoryType::KImage,
        3 => MemoryType::Reserved,
        4 => MemoryType::Mmio,
        5 => MemoryType::PerCpuData,
        6 => MemoryType::BootData,
        7 => MemoryType::BootReclaimable,
        _ => unreachable!("validated boot memory kind"),
    }
}
static TAKEN_OVER: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub(crate) fn complete_takeover() {
    let Some(payload) = inherited_payload() else {
        return;
    };
    assert!(
        !TAKEN_OVER.swap(true, Ordering::AcqRel),
        "boot memory already completed"
    );
    let mut next = crate::mem::MemoryMap::new();
    let mut reclaimed =
        [crate::handoff::ReclaimedMemory::EMPTY; crate::handoff::protocol::MAX_REGIONS];
    let mut reclaimed_count = 0;
    let mut ram = 0usize;
    let mut pools = 0usize;
    for region in crate::mem::memory_map() {
        let region = region.clone();
        if region.memory_type == MemoryType::BootReclaimable {
            let range = crate::boot_resources::PhysicalRange::new(
                region.physical_start as u64,
                region.size_in_bytes as u64,
            );
            let pool_bytes: u64 = payload
                .resource_slice()
                .unwrap()
                .iter()
                .map(|r| {
                    let lo = range.start.max(r.range.start);
                    let hi = range.end().unwrap().min(r.range.end().unwrap());
                    hi.saturating_sub(lo)
                })
                .sum();
            // New boot allocations only replace Free, so cannot split these
            // old pending ranges. Keep an explicit bound if that rule changes.
            *reclaimed
                .get_mut(reclaimed_count)
                .expect("too many pending boot ranges") = crate::handoff::ReclaimedMemory {
                range,
                resource_bytes: pool_bytes,
            };
            reclaimed_count += 1;
            pools += pool_bytes as usize;
            ram += region.size_in_bytes - pool_bytes as usize;
        }
    }
    let mut completed = current_map::<512>().expect("receiver boot map capacity");
    completed.complete_takeover();
    for region in &completed.regions[..completed.len] {
        next.push(MemoryDescriptor::new_with_range(
            region.range.start as usize..region.range.end().unwrap() as usize,
            decode_kind(region.kind),
        ))
        .expect("boot memory map capacity");
    }
    crate::mem::replace_boot_map(next);
    // SAFETY: the boot CPU publishes this once before platform entry/SMP.
    unsafe {
        RECLAIMED.0.get().write(reclaimed);
    }
    RECLAIMED_COUNT.store(reclaimed_count, Ordering::Release);
    for record in &reclaimed[..reclaimed_count] {
        crate::console::_print(format_args!(
            "boot_memory_reclaimed {:#x}..{:#x} ram={} resources={}\n",
            record.range.start,
            record.range.end().unwrap(),
            record.range.len - record.resource_bytes,
            record.resource_bytes
        ));
    }
    crate::console::_print(format_args!(
        "boot_memory_takeover ram={ram} resources={pools}\n"
    ));
    for region in crate::mem::memory_map()
        .iter()
        .filter(|r| r.memory_type != MemoryType::Free)
    {
        crate::console::_print(format_args!(
            "boot_memory_retained {} {:#x}..{:#x}\n",
            region.memory_type,
            region.physical_start,
            region.physical_start + region.size_in_bytes
        ));
    }
}

/// Export the frozen startup map plus ownership transfers completed afterward.
#[cfg(feature = "efi-handoff")]
pub(crate) fn export_map() -> Result<crate::handoff::memory::BootMap, crate::handoff::HandoffError>
{
    let mut map = current_map()?;
    if let Some(range) = crate::boot_payload::returned_pages() {
        map.assign(range, crate::handoff::memory::FREE)?;
    }
    Ok(map)
}

struct ReclaimedStorage(
    UnsafeCell<[crate::handoff::ReclaimedMemory; crate::handoff::protocol::MAX_REGIONS]>,
);
// SAFETY: initialized once by the boot CPU, immutable after release publication.
unsafe impl Sync for ReclaimedStorage {}
static RECLAIMED: ReclaimedStorage = ReclaimedStorage(UnsafeCell::new(
    [crate::handoff::ReclaimedMemory::EMPTY; crate::handoff::protocol::MAX_REGIONS],
));
static RECLAIMED_COUNT: AtomicUsize = AtomicUsize::new(0);
pub(crate) fn reclaimed_memory() -> &'static [crate::handoff::ReclaimedMemory] {
    let count = RECLAIMED_COUNT.load(Ordering::Acquire);
    // SAFETY: publication makes all initialized records immutable and visible.
    unsafe { &(&*RECLAIMED.0.get())[..count] }
}
pub(crate) fn takeover_completed() -> bool {
    TAKEN_OVER.load(Ordering::Acquire)
}

fn current_map<const N: usize>()
-> Result<crate::handoff::memory::BootMap<N>, crate::handoff::HandoffError> {
    use crate::handoff::{memory::BootMap, protocol::MemoryRegion};
    let mut map = BootMap::<N>::new();
    let regions = crate::mem::memory_map();
    if regions.len() > map.regions.len() {
        return Err(crate::handoff::HandoffError::InvalidProtocol);
    }
    for (i, r) in regions.iter().enumerate() {
        map.regions[i] = MemoryRegion {
            range: crate::boot_resources::PhysicalRange::new(
                r.physical_start as u64,
                r.size_in_bytes as u64,
            ),
            kind: encode_kind(r.memory_type),
            reserved: 0,
        };
    }
    map.len = regions.len();
    Ok(map)
}
