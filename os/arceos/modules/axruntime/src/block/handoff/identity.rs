//! Stable host-resource and guest identities used by block passthrough.

use alloc::{collections::BTreeSet, string::String};

use thiserror::Error;

use super::BlockHandoffError;

/// Stable identity of one controller in the shutdown-lifetime runtime registry.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct BlockControllerIdentity {
    runtime_slot: u32,
    generation: u32,
}

impl BlockControllerIdentity {
    pub(super) const fn new(runtime_slot: u32) -> Self {
        Self {
            runtime_slot,
            generation: 1,
        }
    }

    /// Returns the immutable runtime-registry slot.
    pub const fn runtime_slot(self) -> u32 {
        self.runtime_slot
    }

    /// Returns the registry generation.
    ///
    /// Controller hotplug is unsupported, so this is one for every identity.
    pub const fn generation(self) -> u32 {
        self.generation
    }
}

/// Validated host-physical interval used to match guest mappings to devices.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct HostPhysicalRange {
    start: u64,
    end: u64,
}

impl HostPhysicalRange {
    /// Constructs a non-empty half-open interval `[start, start + length)`.
    ///
    /// # Errors
    ///
    /// Returns [`HostPhysicalRangeError::Empty`] for a zero length and
    /// [`HostPhysicalRangeError::Overflow`] when the exclusive end overflows.
    pub const fn new(start: u64, length: u64) -> Result<Self, HostPhysicalRangeError> {
        if length == 0 {
            return Err(HostPhysicalRangeError::Empty);
        }
        let Some(end) = start.checked_add(length) else {
            return Err(HostPhysicalRangeError::Overflow);
        };
        Ok(Self { start, end })
    }

    /// Returns the inclusive base address.
    pub const fn start(self) -> u64 {
        self.start
    }

    /// Returns the interval length.
    pub const fn length(self) -> u64 {
        self.end - self.start
    }

    const fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// Invalid host-physical interval supplied at the virtualization boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum HostPhysicalRangeError {
    /// A device or guest mapping cannot own an empty interval.
    #[error("host physical range is empty")]
    Empty,
    /// The half-open interval end cannot be represented by `u64`.
    #[error("host physical range end overflows")]
    Overflow,
}

/// Stable key of one guest participating in a storage ownership transaction.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct StorageGuestKey(u64);

impl StorageGuestKey {
    /// Wraps a virtualization-layer guest key.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the value supplied by the virtualization layer.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Stable PCI address of a selected block controller.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(C)]
pub struct HostPciEndpoint {
    segment: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl HostPciEndpoint {
    pub(in crate::block) const fn new(segment: u16, bus: u8, device: u8, function: u8) -> Self {
        Self {
            segment,
            bus,
            device,
            function,
        }
    }

    /// Returns the PCI segment group.
    pub const fn segment(self) -> u16 {
        self.segment
    }

    /// Returns the PCI bus number.
    pub const fn bus(self) -> u8 {
        self.bus
    }

    /// Returns the PCI device number.
    pub const fn device(self) -> u8 {
        self.device
    }

    /// Returns the PCI function number.
    pub const fn function(self) -> u8 {
        self.function
    }
}

/// One final guest stage-2 mapping over host-physical device space.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct GuestPassthroughRegion {
    owner: StorageGuestKey,
    range: HostPhysicalRange,
}

impl GuestPassthroughRegion {
    /// Associates a validated host range with its sole candidate guest owner.
    pub const fn new(owner: StorageGuestKey, range: HostPhysicalRange) -> Self {
        Self { owner, range }
    }

    /// Returns the guest key supplied by the virtualization layer.
    pub const fn owner(self) -> StorageGuestKey {
        self.owner
    }

    /// Returns the final host-physical mapping.
    pub const fn range(self) -> HostPhysicalRange {
        self.range
    }
}

pub(super) fn select_controller_owner(
    controller_name: &str,
    resources: &[HostPhysicalRange],
    guest_regions: &[GuestPassthroughRegion],
) -> Result<Option<StorageGuestKey>, BlockHandoffError> {
    if resources.is_empty() {
        return Err(BlockHandoffError::MissingResourceIdentity {
            controller: String::from(controller_name),
        });
    }

    let owners = guest_regions
        .iter()
        .filter(|region| {
            resources
                .iter()
                .any(|resource| resource.overlaps(region.range))
        })
        .map(|region| region.owner)
        .collect::<BTreeSet<_>>();
    let Some(owner) = owners.first().copied() else {
        return Ok(None);
    };
    if owners.len() != 1 {
        return Err(BlockHandoffError::AmbiguousGuestOwners {
            controller: String::from(controller_name),
            owners: owners.into_iter().collect(),
        });
    }
    if resources
        .iter()
        .any(|resource| !owner_covers_resource(owner, *resource, guest_regions))
    {
        return Err(BlockHandoffError::PartialResourceCoverage {
            controller: String::from(controller_name),
            owner,
        });
    }
    Ok(Some(owner))
}

fn owner_covers_resource(
    owner: StorageGuestKey,
    resource: HostPhysicalRange,
    guest_regions: &[GuestPassthroughRegion],
) -> bool {
    let mut cursor = resource.start;
    while cursor < resource.end {
        let Some(next) = guest_regions
            .iter()
            .filter(|region| {
                region.owner == owner && region.range.start <= cursor && cursor < region.range.end
            })
            .map(|region| region.range.end.min(resource.end))
            .max()
        else {
            return false;
        };
        cursor = next;
    }
    true
}
