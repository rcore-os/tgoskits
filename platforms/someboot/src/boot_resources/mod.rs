//! Typed boot-time reservations, independent of image loading and networking.
//!
//! Static plans are executed before firmware services end. Allocations from the
//! resulting arenas use bounded live intervals. Dropping any allocation makes
//! its interval reusable; only explicitly transferred objects survive handoff.

pub const PAGE_SIZE: u64 = 4096;
pub const MAX_RESOURCES: usize = 8;
pub const MAX_ALLOCATIONS: usize = 128;
const PLAN_MAGIC: u64 = 0x5342_5253_504c_414e;

/// Application-defined identity. Zero is reserved for an unused wire record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct ResourceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    Anywhere,
    At(u64),
}

/// Optional immutable content populated while firmware services are active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialContents {
    Empty,
    /// The original firmware boot file; inherited stages retain the same bytes.
    FirmwareBootImage,
}

/// Pointer-free request, safe to read before relocation and allocator setup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct ResourceRequest {
    pub id: ResourceId,
    pub capacity: u64,
    pub alignment: u64,
    address: u64,
    placement: u64,
    contents: u64,
}

impl ResourceRequest {
    pub const fn new(id: ResourceId, capacity: u64, alignment: u64, placement: Placement) -> Self {
        Self {
            id,
            capacity,
            alignment,
            address: match placement {
                Placement::Anywhere => 0,
                Placement::At(addr) => addr,
            },
            placement: match placement {
                Placement::Anywhere => 0,
                Placement::At(_) => 1,
            },
            contents: 0,
        }
    }

    pub const fn placement(self) -> Placement {
        if self.placement == 0 {
            Placement::Anywhere
        } else {
            Placement::At(self.address)
        }
    }

    pub const fn with_initial_contents(mut self, contents: InitialContents) -> Self {
        self.contents = match contents {
            InitialContents::Empty => 0,
            InitialContents::FirmwareBootImage => 1,
        };
        self
    }

    pub const fn initial_contents(self) -> InitialContents {
        if self.contents == 0 {
            InitialContents::Empty
        } else {
            InitialContents::FirmwareBootImage
        }
    }

    pub fn validate(self) -> Result<(), ResourceError> {
        if self.id.0 == 0
            || self.capacity == 0
            || !self.capacity.is_multiple_of(PAGE_SIZE)
            || self.alignment < PAGE_SIZE
            || !self.alignment.is_power_of_two()
            || self.placement > 1
            || self.contents > 1
            || self.placement == 1
                && (self.address == 0
                    || !self.address.is_multiple_of(self.alignment)
                    || self.address.checked_add(self.capacity).is_none())
        {
            return Err(ResourceError::InvalidRequest);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct PhysicalRange {
    pub start: u64,
    pub len: u64,
}

impl PhysicalRange {
    pub const fn new(start: u64, len: u64) -> Self {
        Self { start, len }
    }
    pub fn end(self) -> Option<u64> {
        self.start.checked_add(self.len)
    }
    pub fn valid(self) -> bool {
        self.start != 0 && self.len != 0 && self.end().is_some()
    }
    pub fn contains(self, other: Self) -> bool {
        self.valid() && other.valid() && other.start >= self.start && other.end() <= self.end()
    }
    pub fn overlaps(self, other: Self) -> bool {
        self.valid()
            && other.valid()
            && self.start < other.end().unwrap()
            && other.start < self.end().unwrap()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct ResourceSnapshot {
    pub id: u64,
    pub range: PhysicalRange,
    /// Immutable prefix populated before firmware services end.
    pub initialized: u64,
}

impl ResourceSnapshot {
    pub fn validate(self) -> Result<(), ResourceError> {
        if self.id == 0
            || !self.range.valid()
            || !self.range.start.is_multiple_of(PAGE_SIZE)
            || !self.range.len.is_multiple_of(PAGE_SIZE)
            || self.initialized > self.range.len
        {
            return Err(ResourceError::InvalidSnapshot);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResourceError {
    #[error("invalid boot resource capacity, alignment, placement, or identity")]
    InvalidRequest,
    #[error("invalid boot resource range or live allocation")]
    InvalidSnapshot,
    #[error("duplicate boot resource identity")]
    DuplicateId,
    #[error("boot resource reservations overlap")]
    Overlap,
    #[error("boot resource capacity exceeded")]
    Capacity,
    #[error("boot resource is unavailable")]
    Unavailable,
    #[error("boot resource firmware operation failed (EFI status {0:#x})")]
    Firmware(usize),
}

/// The macro emits one of these records into a retained linker section.
#[doc(hidden)]
#[repr(C)]
pub struct StaticPlan<const N: usize> {
    magic: u64,
    count: u64,
    requests: [ResourceRequest; N],
}
impl<const N: usize> StaticPlan<N> {
    pub const fn new(requests: [ResourceRequest; N]) -> Self {
        Self {
            magic: PLAN_MAGIC,
            count: N as u64,
            requests,
        }
    }
}

/// Declares the application's boot reservations. Invoke once at module scope.
/// Inherited EFI boot requires matching pre-reserved resources; it validates
/// this plan and restores explicitly live allocations without firmware calls.
/// No application callback runs during early boot, and enabling the feature
/// without a declaration does not allocate any application memory.
#[macro_export]
macro_rules! declare_boot_resources {
    ($($request:expr),* $(,)?) => {
        const _: () = {
            #[used]
            #[unsafe(link_section = ".someboot.resources")]
            static PLAN: $crate::boot_resources::StaticPlan<{[$($request),*].len()}> =
                $crate::boot_resources::StaticPlan::new([$($request),*]);
        };
    };
}

#[cfg(feature = "boot-resources")]
mod allocation;
#[cfg(all(feature = "boot-resources", not(target_os = "uefi")))]
mod intervals;
#[cfg(feature = "boot-resources")]
pub use allocation::Allocation;
#[cfg(all(feature = "boot-resources", not(target_os = "uefi")))]
pub use allocation::allocate;
#[cfg(all(feature = "boot-resources", not(target_os = "uefi")))]
pub use allocation::initial_contents;
#[cfg(all(
    feature = "efi-handoff",
    efi,
    target_arch = "loongarch64",
    not(target_os = "uefi")
))]
pub(crate) use allocation::live_snapshots;
#[cfg(all(
    feature = "boot-resources",
    efi,
    target_arch = "loongarch64",
    not(target_os = "uefi")
))]
pub(crate) use allocation::prepare_inherited_plan;
#[cfg(all(feature = "boot-resources", efi, not(target_os = "uefi")))]
pub(crate) use allocation::prepare_static_plan;
#[cfg(all(feature = "boot-resources", target_os = "uefi"))]
pub use allocation::reserve;
#[cfg(all(
    feature = "boot-resources",
    efi,
    target_arch = "loongarch64",
    not(target_os = "uefi")
))]
pub(crate) use allocation::retained_resources;
#[cfg(all(feature = "boot-resources", not(target_os = "uefi")))]
pub(crate) use allocation::{retire_transferred, snapshots};

/// A live object transferred into the next stage; addresses are physical.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct AllocationSnapshot {
    pub resource_id: u64,
    pub range: PhysicalRange,
}
