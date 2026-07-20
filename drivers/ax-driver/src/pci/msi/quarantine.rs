//! Bounded retention for MSI-X resources whose hardware release failed.

use ax_kspin::SpinNoPreempt;
use mmio_api::Mmio;
use pcie::Endpoint;
use rdif_msi::MsiAllocation;
use rdrive::probe::pci::PciAddress;

const PCI_MSI_QUARANTINE_CAPACITY: usize = 64;

/// Capacity failure detected before an MSI provider transfers ownership.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("PCI MSI-X quarantine capacity is exhausted")]
pub(super) struct PciMsiQuarantineCapacity;

/// Why an MSI-X resource set could not be destroyed safely.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PciMsiQuarantineReason {
    SetupContainment,
    ProviderRelease,
    LeaseContainment,
}

/// Complete Rust ownership retained after a failed hardware transaction.
///
/// The endpoint and provider allocation are always present. The table mapping
/// appears after its setup phase transfers ownership.
struct MsiQuarantinedResources {
    address: PciAddress,
    allocation: MsiAllocation,
    table_mmio: Option<Mmio>,
    endpoint: Endpoint,
    reason: PciMsiQuarantineReason,
}

impl MsiQuarantinedResources {
    fn new(
        address: PciAddress,
        allocation: MsiAllocation,
        table_mmio: Option<Mmio>,
        endpoint: Endpoint,
        reason: PciMsiQuarantineReason,
    ) -> Self {
        Self {
            address,
            allocation,
            table_mmio,
            endpoint,
            reason,
        }
    }

    fn diagnostic(&self) -> MsiQuarantineDiagnostic {
        debug_assert_eq!(self.endpoint.address(), self.address);
        MsiQuarantineDiagnostic {
            address: self.address,
            vector_count: self.allocation.vectors().len(),
            retains_table_mapping: self.table_mmio.is_some(),
            retains_endpoint: true,
            reason: self.reason,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MsiQuarantineDiagnostic {
    address: PciAddress,
    vector_count: usize,
    retains_table_mapping: bool,
    retains_endpoint: bool,
    reason: PciMsiQuarantineReason,
}

enum QuarantineSlot<T> {
    Free,
    Reserved,
    Occupied(T),
}

struct QuarantineRegistry<T, const CAPACITY: usize> {
    slots: [QuarantineSlot<T>; CAPACITY],
}

impl<T, const CAPACITY: usize> QuarantineRegistry<T, CAPACITY> {
    const fn new() -> Self {
        Self {
            slots: [const { QuarantineSlot::Free }; CAPACITY],
        }
    }

    fn reserve(&mut self) -> Result<usize, PciMsiQuarantineCapacity> {
        let Some((index, slot)) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| matches!(slot, QuarantineSlot::Free))
        else {
            return Err(PciMsiQuarantineCapacity);
        };
        *slot = QuarantineSlot::Reserved;
        Ok(index)
    }

    fn release(&mut self, index: usize) {
        let slot = self
            .slots
            .get_mut(index)
            .expect("MSI quarantine reservation index is valid");
        assert!(
            matches!(slot, QuarantineSlot::Reserved),
            "only a live MSI quarantine reservation may be released"
        );
        *slot = QuarantineSlot::Free;
    }

    fn retain(&mut self, index: usize, resources: T) {
        let slot = self
            .slots
            .get_mut(index)
            .expect("MSI quarantine reservation index is valid");
        assert!(
            matches!(slot, QuarantineSlot::Reserved),
            "MSI quarantine reservation was already consumed"
        );
        *slot = QuarantineSlot::Occupied(resources);
    }

    fn occupied(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|slot| match slot {
            QuarantineSlot::Occupied(resources) => Some(resources),
            QuarantineSlot::Free | QuarantineSlot::Reserved => None,
        })
    }
}

type PciMsiQuarantineRegistry =
    QuarantineRegistry<MsiQuarantinedResources, PCI_MSI_QUARANTINE_CAPACITY>;

static PCI_MSI_QUARANTINE: SpinNoPreempt<PciMsiQuarantineRegistry> =
    SpinNoPreempt::new(PciMsiQuarantineRegistry::new());

/// One fail-closed slot reserved before vector ownership is accepted.
///
/// Dropping an unconsumed token deliberately leaves the slot reserved. Every
/// ordinary failure path must either prove release and call [`Self::release`]
/// or transfer its complete owner set through [`Self::retain`].
#[must_use = "an MSI quarantine reservation must be released or filled"]
pub(super) struct PciMsiQuarantineReservation {
    slot: Option<usize>,
    address: PciAddress,
}

impl PciMsiQuarantineReservation {
    /// Reserves fail-closed capacity before hardware ownership changes.
    pub(super) fn reserve(address: PciAddress) -> Result<Self, PciMsiQuarantineCapacity> {
        let slot = PCI_MSI_QUARANTINE.lock().reserve()?;
        Ok(Self {
            slot: Some(slot),
            address,
        })
    }

    /// Releases capacity after every associated hardware owner was destroyed.
    pub(super) fn release(mut self) {
        let slot = self
            .slot
            .take()
            .expect("MSI quarantine reservation is consumed exactly once");
        PCI_MSI_QUARANTINE.lock().release(slot);
    }

    /// Permanently retains a resource set whose hardware state is uncertain.
    pub(super) fn retain(
        mut self,
        allocation: MsiAllocation,
        table_mmio: Option<Mmio>,
        endpoint: Endpoint,
        reason: PciMsiQuarantineReason,
    ) {
        let resources =
            MsiQuarantinedResources::new(self.address, allocation, table_mmio, endpoint, reason);
        let diagnostic = resources.diagnostic();
        let slot = self
            .slot
            .take()
            .expect("MSI quarantine reservation is consumed exactly once");
        let retained = {
            let mut registry = PCI_MSI_QUARANTINE.lock();
            registry.retain(slot, resources);
            registry.occupied().count()
        };
        log::error!(
            "quarantined PCI MSI-X resources at {}: reason={:?}, vectors={}, table_mapping={}, \
             endpoint={}, retained_sets={retained}",
            diagnostic.address,
            diagnostic.reason,
            diagnostic.vector_count,
            diagnostic.retains_table_mapping,
            diagnostic.retains_endpoint,
        );
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[test]
    fn capacity_is_reserved_before_an_owner_can_be_retained() {
        let mut registry = QuarantineRegistry::<usize, 2>::new();
        let first = registry.reserve().unwrap();
        let second = registry.reserve().unwrap();

        assert_eq!(registry.reserve(), Err(PciMsiQuarantineCapacity));
        registry.release(first);
        assert!(registry.reserve().is_ok());
        registry.retain(second, 7);
        assert_eq!(
            registry.occupied().copied().collect::<alloc::vec::Vec<_>>(),
            [7]
        );
    }

    #[test]
    fn retained_slot_keeps_rust_ownership_alive() {
        let dropped = Cell::new(false);
        let mut registry = QuarantineRegistry::<DropProbe<'_>, 1>::new();
        let slot = registry.reserve().unwrap();

        registry.retain(slot, DropProbe(&dropped));

        assert!(!dropped.get());
        assert_eq!(registry.occupied().count(), 1);
    }

    struct DropProbe<'a>(&'a Cell<bool>);

    impl Drop for DropProbe<'_> {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }
}
