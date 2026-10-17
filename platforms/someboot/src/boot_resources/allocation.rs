use core::sync::atomic::{AtomicU64, Ordering};

use super::*;
pub(crate) fn validate_requests(requests: &[ResourceRequest]) -> Result<(), ResourceError> {
    if requests.len() > MAX_RESOURCES {
        return Err(ResourceError::Capacity);
    }
    for (i, request) in requests.iter().enumerate() {
        request.validate()?;
        for other in &requests[..i] {
            if request.id == other.id {
                return Err(ResourceError::DuplicateId);
            }
            if let (Placement::At(a), Placement::At(b)) = (request.placement(), other.placement())
                && PhysicalRange::new(a, request.capacity)
                    .overlaps(PhysicalRange::new(b, other.capacity))
            {
                return Err(ResourceError::Overlap);
            }
        }
    }
    Ok(())
}

/// A single EFI page allocator is used by provisioning and native UEFI callers.
///
/// # Safety
/// Successful allocations must be exclusive, page-aligned physical memory of
/// the requested capacity and alignment. `release` relinquishes that ownership.
pub(super) unsafe trait PageAllocator {
    fn reserve(&mut self, request: ResourceRequest) -> Result<PhysicalRange, ResourceError>;
    unsafe fn release(&mut self, range: PhysicalRange);
}

/// Provision atomically: partial failures release earlier reservations in reverse.
pub(super) fn provision<A: PageAllocator>(
    requests: &[ResourceRequest],
    allocator: &mut A,
) -> Result<[ResourceSnapshot; MAX_RESOURCES], ResourceError> {
    validate_requests(requests)?;
    let mut records = [ResourceSnapshot::default(); MAX_RESOURCES];
    for (index, request) in requests.iter().enumerate() {
        let result = allocator.reserve(*request).and_then(|range| {
            if !range.valid()
                || range.len != request.capacity
                || !range.start.is_multiple_of(request.alignment)
                || matches!(request.placement(), Placement::At(addr) if addr != range.start)
                || records[..index]
                    .iter()
                    .any(|record| record.range.overlaps(range))
            {
                // SAFETY: the allocator just transferred ownership of this range.
                unsafe {
                    allocator.release(range);
                }
                return Err(ResourceError::InvalidSnapshot);
            }
            Ok(range)
        });
        match result {
            Ok(range) => {
                records[index] = ResourceSnapshot {
                    id: request.id.0,
                    range,
                    used: 0,
                    initialized: 0,
                }
            }
            Err(error) => {
                for record in records[..index].iter().rev() {
                    // SAFETY: these successful reservations are not yet published.
                    unsafe {
                        allocator.release(record.range);
                    }
                }
                return Err(error);
            }
        }
    }
    Ok(records)
}

#[derive(Debug)]
struct Arena {
    id: AtomicU64,
    start: AtomicU64,
    len: AtomicU64,
    used: AtomicU64,
    #[cfg(not(target_os = "uefi"))]
    initialized: AtomicU64,
}
impl Arena {
    const fn new() -> Self {
        Self {
            id: AtomicU64::new(0),
            start: AtomicU64::new(0),
            len: AtomicU64::new(0),
            used: AtomicU64::new(0),
            #[cfg(not(target_os = "uefi"))]
            initialized: AtomicU64::new(0),
        }
    }
    #[cfg(not(target_os = "uefi"))]
    fn snapshot(&self) -> ResourceSnapshot {
        ResourceSnapshot {
            id: self.id.load(Ordering::Acquire),
            range: PhysicalRange::new(
                self.start.load(Ordering::Relaxed),
                self.len.load(Ordering::Relaxed),
            ),
            used: self.used.load(Ordering::Acquire),
            initialized: self.initialized.load(Ordering::Relaxed),
        }
    }
}
static ARENAS: [Arena; MAX_RESOURCES] = [const { Arena::new() }; MAX_RESOURCES];

/// Owns a contiguous reservation. The address is physical; mapping and access
/// require the platform's normal memory-safety contract. This value is not Clone.
#[derive(Debug)]
pub struct Allocation {
    range: PhysicalRange,
    #[cfg(target_os = "uefi")]
    initialized: u64,
    owner: AllocationOwner,
}
#[derive(Debug)]
enum AllocationOwner {
    Arena {
        index: usize,
        previous: u64,
        end: u64,
    },
    #[cfg(target_os = "uefi")]
    Firmware,
}
impl Allocation {
    pub fn range(&self) -> PhysicalRange {
        self.range
    }
    #[cfg(target_os = "uefi")]
    pub fn initial_contents(&self) -> &[u8] {
        // SAFETY: this guard owns the reservation and its initialized prefix;
        // the borrow prevents release while the immutable bytes are in use.
        unsafe {
            core::slice::from_raw_parts(self.range.start as *const u8, self.initialized as usize)
        }
    }
}
impl Drop for Allocation {
    fn drop(&mut self) {
        match self.owner {
            AllocationOwner::Arena {
                index,
                previous,
                end,
            } => {
                let _ = ARENAS[index].used.compare_exchange(
                    end,
                    previous,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            #[cfg(target_os = "uefi")]
            AllocationOwner::Firmware => {
                // SAFETY: this guard is the unique owner of the firmware pages.
                unsafe {
                    firmware::release(self.range);
                }
            }
        }
    }
}

/// Allocates from an application-declared arena, without firmware calls.
#[cfg(feature = "boot-resources")]
pub fn allocate(id: ResourceId, bytes: u64, alignment: u64) -> Result<Allocation, ResourceError> {
    if id.0 == 0 || bytes == 0 || !alignment.is_power_of_two() {
        return Err(ResourceError::InvalidRequest);
    }
    let (index, arena) = ARENAS
        .iter()
        .enumerate()
        .find(|(_, arena)| arena.id.load(Ordering::Acquire) == id.0)
        .ok_or(ResourceError::Unavailable)?;
    let start = arena.start.load(Ordering::Relaxed);
    let capacity = arena.len.load(Ordering::Relaxed);
    let mut previous = arena.used.load(Ordering::Acquire);
    loop {
        let address = start
            .checked_add(previous)
            .and_then(|v| v.checked_add(alignment - 1))
            .map(|v| v & !(alignment - 1))
            .ok_or(ResourceError::Capacity)?;
        let end = address
            .checked_sub(start)
            .and_then(|v| v.checked_add(bytes))
            .filter(|end| *end <= capacity)
            .ok_or(ResourceError::Capacity)?;
        match arena
            .used
            .compare_exchange(previous, end, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {
                return Ok(Allocation {
                    range: PhysicalRange::new(address, bytes),
                    #[cfg(target_os = "uefi")]
                    initialized: 0,
                    owner: AllocationOwner::Arena {
                        index,
                        previous,
                        end,
                    },
                });
            }
            Err(next) => previous = next,
        }
    }
}

#[cfg(not(target_os = "uefi"))]
pub(crate) fn snapshots() -> [ResourceSnapshot; MAX_RESOURCES] {
    core::array::from_fn(|i| ARENAS[i].snapshot())
}

/// # Safety
/// Boot CPU only, before allocations or other CPUs can observe this state. The
/// ranges must remain reserved and exclusively owned across the boot stages.
#[cfg(not(target_os = "uefi"))]
pub(crate) unsafe fn restore(records: &[ResourceSnapshot]) -> Result<(), ResourceError> {
    if records.len() > MAX_RESOURCES {
        return Err(ResourceError::Capacity);
    }
    for (i, record) in records.iter().enumerate() {
        record.validate()?;
        if records[..i].iter().any(|other| other.id == record.id) {
            return Err(ResourceError::DuplicateId);
        }
        if records[..i]
            .iter()
            .any(|other| other.range.overlaps(record.range))
        {
            return Err(ResourceError::Overlap);
        }
    }
    for (arena, record) in ARENAS.iter().zip(records) {
        arena.start.store(record.range.start, Ordering::Relaxed);
        arena.len.store(record.range.len, Ordering::Relaxed);
        arena.used.store(record.used, Ordering::Relaxed);
        arena
            .initialized
            .store(record.initialized, Ordering::Relaxed);
        arena.id.store(record.id, Ordering::Release);
    }
    Ok(())
}

#[cfg(any(target_os = "uefi", all(efi, feature = "boot-resources")))]
#[path = "firmware.rs"]
pub(crate) mod firmware;

/// Reserves pages while native UEFI Boot Services are active.
#[cfg(all(target_os = "uefi", feature = "boot-resources"))]
pub fn reserve(request: ResourceRequest) -> Result<Allocation, ResourceError> {
    let mut records = provision(&[request], &mut firmware::FirmwarePages)?;
    initialize_reservations(&[request], &mut records, &mut firmware::FirmwarePages)?;
    Ok(Allocation {
        range: records[0].range,
        initialized: records[0].initialized,
        owner: AllocationOwner::Firmware,
    })
}

#[cfg(all(not(target_os = "uefi"), efi, feature = "boot-resources"))]
pub(crate) fn prepare_static_plan() {
    let requests = static_requests();
    let mut records = provision(requests, &mut firmware::FirmwarePages)
        .expect("failed to provision boot resource plan");
    initialize_reservations(requests, &mut records, &mut firmware::FirmwarePages)
        .expect("failed to initialize boot resources");
    // SAFETY: this is the only early-boot initialization, before EBS and SMP.
    unsafe {
        restore(&records[..requests.len()]).expect("invalid provisioned resources");
    }
}

#[cfg(all(not(target_os = "uefi"), efi))]
fn static_requests() -> &'static [ResourceRequest] {
    unsafe extern "C" {
        static __someboot_resources_start: u8;
        static __someboot_resources_end: u8;
    }
    let start = core::ptr::addr_of!(__someboot_resources_start);
    let end = core::ptr::addr_of!(__someboot_resources_end);
    let len = end as usize - start as usize;
    if len == 0 {
        return &[];
    }
    assert!(
        len >= 16 && (start as usize).is_multiple_of(8),
        "invalid boot resource plan header"
    );
    // SAFETY: the linker retains this pointer-free read-only application section.
    let (magic, count) = unsafe {
        (
            start.cast::<u64>().read(),
            start.cast::<u64>().add(1).read(),
        )
    };
    assert_eq!(magic, PLAN_MAGIC, "invalid boot resource plan magic");
    let count = usize::try_from(count).expect("boot resource count overflows");
    assert!(
        count <= MAX_RESOURCES && len == 16 + count * core::mem::size_of::<ResourceRequest>(),
        "invalid or multiple boot resource plans"
    );
    // SAFETY: the complete static request array is bounded by the linker section.
    unsafe { core::slice::from_raw_parts(start.add(16).cast::<ResourceRequest>(), count) }
}

/// Restores inherited ownership and rejects a plan the producer did not reserve.
/// # Safety
/// Boot CPU only, before any allocation or SMP; ranges remain reserved.
#[cfg(all(efi, target_arch = "loongarch64", not(target_os = "uefi")))]
pub(crate) unsafe fn prepare_inherited_plan(
    records: &[ResourceSnapshot],
) -> Result<(), ResourceError> {
    validate_inherited_requests(static_requests(), records)?;
    // SAFETY: forwarded from the one-time boot ownership contract.
    unsafe { restore(records) }
}

#[cfg(any(test, all(efi, target_arch = "loongarch64", not(target_os = "uefi"))))]
fn validate_inherited_requests(
    requests: &[ResourceRequest],
    records: &[ResourceSnapshot],
) -> Result<(), ResourceError> {
    validate_requests(requests)?;
    for request in requests {
        let record = records
            .iter()
            .find(|r| r.id == request.id.0)
            .ok_or(ResourceError::Unavailable)?;
        record.validate()?;
        if record.range.len < request.capacity
            || !record.range.start.is_multiple_of(request.alignment)
            || matches!(request.placement(), Placement::At(addr) if addr != record.range.start)
            || request.initial_contents() == InitialContents::FirmwareBootImage
                && record.initialized == 0
        {
            return Err(ResourceError::InvalidSnapshot);
        }
    }
    Ok(())
}

#[cfg(all(feature = "boot-resources", not(target_os = "uefi")))]
pub fn initial_contents(id: ResourceId) -> Option<&'static [u8]> {
    let arena = ARENAS
        .iter()
        .find(|arena| arena.id.load(Ordering::Acquire) == id.0 && id.0 != 0)?;
    let len = arena.initialized.load(Ordering::Relaxed);
    if len == 0 {
        return None;
    }
    let address = arena.start.load(Ordering::Relaxed);
    let pointer = crate::mem::phys_to_virt(address as usize);
    // SAFETY: the initialized prefix is immutable and permanently reserved.
    // Allocation starts after this prefix; restoration occurs only before SMP.
    Some(unsafe { core::slice::from_raw_parts(pointer.cast_const(), len as usize) })
}

/// Completes initial contents before publishing any reservation. Failure frees
/// the entire unpublished plan, including previously initialized resources.
#[cfg(any(target_os = "uefi", all(efi, feature = "boot-resources")))]
fn initialize_reservations<A: PageAllocator>(
    requests: &[ResourceRequest],
    records: &mut [ResourceSnapshot; MAX_RESOURCES],
    allocator: &mut A,
) -> Result<(), ResourceError> {
    for (request, record) in requests.iter().zip(records.iter_mut()) {
        if request.initial_contents() == InitialContents::FirmwareBootImage {
            // SAFETY: provisioning returned exclusive reservations, not published
            // until every initializer has succeeded.
            match unsafe { firmware::initialize_image(record.range) } {
                Ok(len) => {
                    record.initialized = len;
                    record.used = len;
                }
                Err(error) => {
                    for record in records[..requests.len()].iter().rev() {
                        // SAFETY: no reservation has been published or exposed yet.
                        unsafe { allocator.release(record.range) };
                    }
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;
    struct Pages {
        next: u64,
        remaining: usize,
        released: alloc::vec::Vec<PhysicalRange>,
    }
    // SAFETY: test addresses model exclusive page-aligned reservations; no memory is accessed.
    unsafe impl PageAllocator for Pages {
        fn reserve(&mut self, r: ResourceRequest) -> Result<PhysicalRange, ResourceError> {
            if self.remaining == 0 {
                return Err(ResourceError::Firmware(7));
            }
            self.remaining -= 1;
            let range = PhysicalRange::new(self.next, r.capacity);
            self.next += r.capacity;
            Ok(range)
        }
        unsafe fn release(&mut self, range: PhysicalRange) {
            self.released.push(range);
        }
    }
    fn request(id: u64) -> ResourceRequest {
        ResourceRequest::new(ResourceId(id), 4096, 4096, Placement::Anywhere)
    }
    #[test]
    fn provisioning_is_atomic_and_validates_before_allocation() {
        let mut pages = Pages {
            next: 0x1000,
            remaining: 1,
            released: alloc::vec::Vec::new(),
        };
        assert_eq!(
            provision(&[request(1), request(2)], &mut pages),
            Err(ResourceError::Firmware(7))
        );
        assert_eq!(pages.released, [PhysicalRange::new(0x1000, 4096)]);
        assert_eq!(
            provision(&[request(1), request(1)], &mut pages),
            Err(ResourceError::DuplicateId)
        );
        assert_eq!(pages.remaining, 0);
        assert!(provision(&[], &mut pages).is_ok());
    }
    #[test]
    fn rejects_bad_layout_and_watermarks() {
        assert_eq!(
            ResourceRequest::new(ResourceId(1), 4096, 3, Placement::Anywhere).validate(),
            Err(ResourceError::InvalidRequest)
        );
        let requests = [
            ResourceRequest::new(ResourceId(1), 8192, 4096, Placement::At(4096)),
            ResourceRequest::new(ResourceId(2), 4096, 4096, Placement::At(8192)),
        ];
        assert_eq!(validate_requests(&requests), Err(ResourceError::Overlap));
        let record = ResourceSnapshot {
            id: 1,
            range: PhysicalRange::new(0x1000, 4096),
            used: 128,
            initialized: 0,
        };
        assert!(validate_inherited_requests(&[request(1)], &[record]).is_ok());
        assert_eq!(
            validate_inherited_requests(&[request(2)], &[record]),
            Err(ResourceError::Unavailable)
        );
        assert_eq!(
            ResourceSnapshot {
                initialized: record.used + 1,
                ..record
            }
            .validate(),
            Err(ResourceError::InvalidSnapshot)
        );
        assert_eq!(
            ResourceSnapshot {
                id: 1,
                range: PhysicalRange::new(4096, 4096),
                initialized: 0,
                used: 4097
            }
            .validate(),
            Err(ResourceError::InvalidSnapshot)
        );
    }
}
