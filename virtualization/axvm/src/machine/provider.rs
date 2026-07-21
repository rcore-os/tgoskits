//! Provider-local firmware references and lease-backed assignment grants.

use alloc::{string::String, vec::Vec};
use core::num::NonZeroU32;

/// Access semantics of a reference to a host firmware provider.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HostProviderReferenceKind {
    /// A descriptive hierarchy edge such as an FDT parent node.
    Hierarchy,
    /// An interrupt route handled by the VM interrupt topology.
    InterruptRoute,
    /// One clock selected from a clock provider.
    Clock,
    /// One reset line selected from a reset controller.
    Reset,
    /// Boot-time clock configuration that is either pinned by the platform or
    /// replayed through a VM-local mediator.
    ClockConfiguration,
    /// One provider-managed resource such as a DMA channel or power domain.
    ManagedSubresource,
}

/// A provider reference together with its provider-local resource selector.
///
/// Keeping the selector cells allows mediated providers to grant individual
/// clocks, resets, and other resources without granting a complete controller
/// register aperture.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostProviderReference {
    kind: HostProviderReferenceKind,
    specifier: Vec<u32>,
}

impl HostProviderReference {
    /// Creates a hierarchy reference with no provider-local selector.
    pub const fn hierarchy() -> Self {
        Self {
            kind: HostProviderReferenceKind::Hierarchy,
            specifier: Vec::new(),
        }
    }

    /// Creates an interrupt-route reference.
    pub const fn interrupt_route(specifier: Vec<u32>) -> Self {
        Self {
            kind: HostProviderReferenceKind::InterruptRoute,
            specifier,
        }
    }

    /// Creates a reference to one provider-local clock.
    pub const fn clock(specifier: Vec<u32>) -> Self {
        Self {
            kind: HostProviderReferenceKind::Clock,
            specifier,
        }
    }

    /// Creates a reference to one provider-local reset line.
    pub const fn reset(specifier: Vec<u32>) -> Self {
        Self {
            kind: HostProviderReferenceKind::Reset,
            specifier,
        }
    }

    /// Creates a reference used only to configure clocks during firmware boot.
    pub const fn clock_configuration(specifier: Vec<u32>) -> Self {
        Self {
            kind: HostProviderReferenceKind::ClockConfiguration,
            specifier,
        }
    }

    /// Creates a reference to one resource managed by a provider.
    pub const fn managed_subresource(specifier: Vec<u32>) -> Self {
        Self {
            kind: HostProviderReferenceKind::ManagedSubresource,
            specifier,
        }
    }

    /// Returns the access semantics of this provider reference.
    pub const fn kind(&self) -> HostProviderReferenceKind {
        self.kind
    }

    /// Returns the provider-local selector cells without the phandle.
    pub fn specifier(&self) -> &[u32] {
        &self.specifier
    }

    pub(crate) const fn is_managed(&self) -> bool {
        matches!(
            self.kind,
            HostProviderReferenceKind::Clock
                | HostProviderReferenceKind::Reset
                | HostProviderReferenceKind::ClockConfiguration
                | HostProviderReferenceKind::ManagedSubresource
        )
    }
}

/// Whether a firmware dependency is necessary to expose a physical device.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostDeviceDependencyKind {
    /// The consumer cannot be represented when the provider is unavailable.
    Required,
    /// The capability may be omitted while preserving a safe device model.
    Optional,
}

/// One firmware dependency from a host device to a provider node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostDeviceDependency {
    provider: super::HostDeviceId,
    property: String,
    kind: HostDeviceDependencyKind,
    reference: HostProviderReference,
}

impl HostDeviceDependency {
    /// Creates a checked firmware dependency.
    pub fn new(
        provider: super::HostDeviceId,
        property: impl Into<String>,
        kind: HostDeviceDependencyKind,
        reference: HostProviderReference,
    ) -> super::MachinePlanResult<Self> {
        let property = property.into();
        if property.trim().is_empty() {
            return Err(super::MachinePlanError::EmptyIdentifier {
                kind: "host device dependency property",
            });
        }
        Ok(Self {
            provider,
            property,
            kind,
            reference,
        })
    }

    /// Returns the stable identity of the provider node.
    pub const fn provider(&self) -> &super::HostDeviceId {
        &self.provider
    }

    /// Returns the firmware property containing the reference.
    pub fn property(&self) -> &str {
        &self.property
    }

    /// Returns whether the provider is required or optional.
    pub const fn kind(&self) -> HostDeviceDependencyKind {
        self.kind
    }

    /// Returns the provider access semantics and provider-local selector.
    pub const fn reference(&self) -> &HostProviderReference {
        &self.reference
    }
}

/// Stable state exported for one provider-local resource.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HostProviderResourceState {
    /// An enabled clock whose rate remains fixed for the lease lifetime.
    FixedClock(NonZeroU32),
    /// A reset line that remains deasserted for the lease lifetime.
    DeassertedReset,
    /// A clock whose operations are exposed through a VM-local mediator.
    MediatedClock,
    /// A reset line whose operations are exposed through a VM-local mediator.
    MediatedReset,
}

/// Trusted assignment authority for one provider-local host resource.
///
/// This is not an observation of current hardware state. For fixed resources,
/// the platform adapter must pin the described state for the complete lease.
/// For mediated resources, the claim provider must return a matching runtime
/// capability whose handle structurally retains that lease.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostProviderResourceGrant {
    reference: HostProviderReference,
    state: HostProviderResourceState,
}

impl HostProviderResourceGrant {
    /// Grants one enabled, fixed-rate clock to a static guest device.
    pub const fn fixed_clock(specifier: Vec<u32>, rate_hz: NonZeroU32) -> Self {
        Self {
            reference: HostProviderReference::clock(specifier),
            state: HostProviderResourceState::FixedClock(rate_hz),
        }
    }

    /// Grants one reset line that stays deasserted for the guest-device lease.
    pub const fn deasserted_reset(specifier: Vec<u32>) -> Self {
        Self {
            reference: HostProviderReference::reset(specifier),
            state: HostProviderResourceState::DeassertedReset,
        }
    }

    /// Grants mutable access to one clock through a VM-local mediator.
    pub const fn mediated_clock(specifier: Vec<u32>) -> Self {
        Self {
            reference: HostProviderReference::clock(specifier),
            state: HostProviderResourceState::MediatedClock,
        }
    }

    /// Grants mutable access to one reset line through a VM-local mediator.
    pub const fn mediated_reset(specifier: Vec<u32>) -> Self {
        Self {
            reference: HostProviderReference::reset(specifier),
            state: HostProviderResourceState::MediatedReset,
        }
    }

    /// Returns the provider-local resource identity covered by this grant.
    pub const fn reference(&self) -> &HostProviderReference {
        &self.reference
    }

    /// Returns the stable state guaranteed by the platform adapter.
    pub const fn state(&self) -> HostProviderResourceState {
        self.state
    }

    pub(crate) fn fold_generation(&self, seed: u64, provider: &str) -> u64 {
        let mut hash = hash_bytes(seed, &(provider.len() as u64).to_le_bytes());
        hash = hash_bytes(hash, provider.as_bytes());
        hash = hash_bytes(hash, &[reference_kind_tag(self.reference.kind)]);
        hash = hash_bytes(hash, &(self.reference.specifier.len() as u64).to_le_bytes());
        for cell in &self.reference.specifier {
            hash = hash_bytes(hash, &cell.to_le_bytes());
        }
        match self.state {
            HostProviderResourceState::FixedClock(rate_hz) => {
                hash = hash_bytes(hash, &[0]);
                hash_bytes(hash, &rate_hz.get().to_le_bytes())
            }
            HostProviderResourceState::DeassertedReset => hash_bytes(hash, &[1]),
            HostProviderResourceState::MediatedClock => hash_bytes(hash, &[2]),
            HostProviderResourceState::MediatedReset => hash_bytes(hash, &[3]),
        }
    }
}

/// One provider-local resource that must be retained by the VM ownership
/// transaction.
///
/// This claim is derived from a trusted snapshot grant by the machine planner;
/// device models cannot construct or consume it directly.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostProviderResourceClaim {
    provider: super::HostDeviceId,
    grant: HostProviderResourceGrant,
}

impl HostProviderResourceClaim {
    pub(crate) const fn new(
        provider: super::HostDeviceId,
        grant: HostProviderResourceGrant,
    ) -> Self {
        Self { provider, grant }
    }

    /// Returns the physical provider that owns the selected resource.
    pub const fn provider(&self) -> &super::HostDeviceId {
        &self.provider
    }

    /// Returns the provider-local identity and stable state being retained.
    pub const fn grant(&self) -> &HostProviderResourceGrant {
        &self.grant
    }
}

fn reference_kind_tag(kind: HostProviderReferenceKind) -> u8 {
    match kind {
        HostProviderReferenceKind::Hierarchy => 0,
        HostProviderReferenceKind::InterruptRoute => 1,
        HostProviderReferenceKind::Clock => 2,
        HostProviderReferenceKind::Reset => 3,
        HostProviderReferenceKind::ClockConfiguration => 4,
        HostProviderReferenceKind::ManagedSubresource => 5,
    }
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
    }
    hash
}
