//! Typed ownership transaction for host block-controller passthrough.

mod identity;
mod transaction;

use alloc::{boxed::Box, collections::BTreeSet, vec::Vec};
use core::fmt;

use identity::select_controller_owner;
pub use identity::*;
use thiserror::Error;
#[cfg(test)]
use transaction::{GuestControllerTransaction, PreparedControllerTransaction};
use transaction::{commit_controller_batch, return_controller_batch};

use super::controller::{
    BlockHandoffError, ControllerHandoffReservation, GuestOwnedControllerLease,
    QuarantinedControllerLease, runtime_handoff_controllers,
};

/// Proof that no guest execution path can issue new controller operations.
#[derive(Debug)]
#[must_use = "controller return requires an explicit guest-access revocation proof"]
pub struct GuestAccessRevoked {
    _private: (),
}

impl GuestAccessRevoked {
    /// Creates a proof at the virtualization ownership boundary.
    ///
    /// # Safety
    ///
    /// Every guest that could access the controllers represented by the
    /// associated [`GuestOwnedBlockControllers`] must be stopped. Guest IRQ
    /// injection and forwarding routes must be removed and synchronized, and
    /// guest MMIO mappings plus device-emulation callbacks must be revoked.
    /// Residual DMA programmed before revocation may still be active; the
    /// controller-return transaction owns reset, DMA quiescence, and
    /// reinitialization before republishing host access.
    pub const unsafe fn new() -> Self {
        Self { _private: () }
    }
}

/// Reversible reservation of every interrupt-backed runtime controller.
#[must_use = "dropping this permit cancels only the non-destructive reservation"]
pub struct PreparedBlockHandoff {
    controllers: Vec<ControllerHandoffReservation>,
    selected_guest_keys: Box<[StorageGuestKey]>,
    selected_pci_endpoints: Box<[HostPciEndpoint]>,
}

impl PreparedBlockHandoff {
    /// Returns whether no block controller matched the guest mappings.
    pub fn is_empty(&self) -> bool {
        self.controllers.is_empty()
    }

    /// Returns the exact guest owners selected from final host mappings.
    pub fn selected_guest_keys(&self) -> &[StorageGuestKey] {
        &self.selected_guest_keys
    }

    /// Returns selected controllers that were discovered through PCI.
    pub fn selected_pci_endpoints(&self) -> &[HostPciEndpoint] {
        &self.selected_pci_endpoints
    }

    /// Returns the controllers reserved by this transaction.
    pub fn identities(&self) -> impl ExactSizeIterator<Item = BlockControllerIdentity> + '_ {
        self.controllers
            .iter()
            .map(|controller| controller.identity())
    }

    /// Closes admission and transfers every reserved controller to guest ownership.
    ///
    /// A partial failure quarantines every controller that crossed the commit
    /// boundary and cancels the untouched reservations.
    pub fn commit(self) -> Result<GuestOwnedBlockControllers, BlockHandoffCommitFailure> {
        commit_controller_batch(self.controllers)
            .map(|controllers| GuestOwnedBlockControllers {
                controllers: controllers.into_boxed_slice(),
            })
            .map_err(|failure| BlockHandoffCommitFailure {
                source: failure.source,
                quarantined: QuarantinedBlockControllers::new(failure.quarantined),
                canceled: failure.canceled.into_boxed_slice(),
            })
    }
}

impl fmt::Debug for PreparedBlockHandoff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedBlockHandoff")
            .field("identities", &self.identities().collect::<Vec<_>>())
            .field("selected_guest_keys", &self.selected_guest_keys)
            .field("selected_pci_endpoints", &self.selected_pci_endpoints)
            .finish()
    }
}

/// Controllers exclusively owned by a stopped-or-running passthrough guest.
#[must_use = "guest-owned controllers must be returned or retained fail-closed"]
pub struct GuestOwnedBlockControllers {
    controllers: Box<[GuestOwnedControllerLease]>,
}

impl GuestOwnedBlockControllers {
    /// Returns the controller identities carried by this ownership token.
    pub fn identities(&self) -> impl ExactSizeIterator<Item = BlockControllerIdentity> + '_ {
        self.controllers
            .iter()
            .map(|controller| controller.identity())
    }

    /// Reinitializes every controller after guest routing has been revoked.
    pub fn return_to_host(
        self,
        _revoked: GuestAccessRevoked,
    ) -> Result<HostRunningBlockControllers, BlockHandoffReturnFailure> {
        return_controller_batch(self.controllers.into_vec())
            .map(|controllers| HostRunningBlockControllers {
                controllers: controllers.into_boxed_slice(),
            })
            .map_err(|failure| BlockHandoffReturnFailure {
                source: failure.source,
                returned: failure.returned.into_boxed_slice(),
                quarantined: QuarantinedBlockControllers::new(failure.quarantined),
            })
    }
}

impl fmt::Debug for GuestOwnedBlockControllers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuestOwnedBlockControllers")
            .field("identities", &self.identities().collect::<Vec<_>>())
            .finish()
    }
}

/// Controllers that completed reset, reinitialization, and IRQ reopening.
#[derive(Debug)]
pub struct HostRunningBlockControllers {
    controllers: Box<[BlockControllerIdentity]>,
}

impl HostRunningBlockControllers {
    /// Returns the identities republished for host service.
    pub fn identities(&self) -> impl ExactSizeIterator<Item = BlockControllerIdentity> + '_ {
        self.controllers.iter().copied()
    }
}

/// Runtime owners deliberately retained after a destructive failure.
#[must_use = "quarantined controller owners must remain retained for diagnostics"]
pub struct QuarantinedBlockControllers {
    controllers: Box<[QuarantinedControllerLease]>,
}

impl QuarantinedBlockControllers {
    fn new(controllers: Vec<QuarantinedControllerLease>) -> Self {
        Self {
            controllers: controllers.into_boxed_slice(),
        }
    }

    /// Returns every controller retained offline by this token.
    pub fn identities(&self) -> impl ExactSizeIterator<Item = BlockControllerIdentity> + '_ {
        self.controllers
            .iter()
            .map(|controller| controller.identity())
    }
}

impl fmt::Debug for QuarantinedBlockControllers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let controllers = self
            .controllers
            .iter()
            .map(|controller| (controller.identity(), controller.controller_name()))
            .collect::<Vec<_>>();
        formatter
            .debug_struct("QuarantinedBlockControllers")
            .field("controllers", &controllers)
            .finish()
    }
}

/// A destructive commit failure with explicit quarantine and cancellation sets.
#[derive(Debug, Error)]
#[error("block-controller handoff commit failed: {source}")]
pub struct BlockHandoffCommitFailure {
    source: BlockHandoffError,
    quarantined: QuarantinedBlockControllers,
    canceled: Box<[BlockControllerIdentity]>,
}

impl BlockHandoffCommitFailure {
    /// Returns the controller error that stopped the commit.
    pub const fn source_error(&self) -> &BlockHandoffError {
        &self.source
    }

    /// Returns controllers that crossed the destructive boundary.
    pub const fn quarantined(&self) -> &QuarantinedBlockControllers {
        &self.quarantined
    }

    /// Returns untouched controller reservations canceled after the failure.
    pub fn canceled_identities(&self) -> &[BlockControllerIdentity] {
        &self.canceled
    }

    /// Retains the fail-closed controller ownership record separately.
    pub fn into_quarantine(self) -> QuarantinedBlockControllers {
        self.quarantined
    }
}

/// A guest-return failure that distinguishes restored and quarantined controllers.
#[derive(Debug, Error)]
#[error("block-controller guest return failed: {source}")]
pub struct BlockHandoffReturnFailure {
    source: BlockHandoffError,
    returned: Box<[BlockControllerIdentity]>,
    quarantined: QuarantinedBlockControllers,
}

impl BlockHandoffReturnFailure {
    /// Returns the first controller error observed while completing all returns.
    pub const fn source_error(&self) -> &BlockHandoffError {
        &self.source
    }

    /// Returns controllers already republished for host service.
    pub fn returned_identities(&self) -> &[BlockControllerIdentity] {
        &self.returned
    }

    /// Returns controllers retained offline after failed reinitialization.
    pub const fn quarantined(&self) -> &QuarantinedBlockControllers {
        &self.quarantined
    }

    /// Retains the fail-closed controller ownership record separately.
    pub fn into_quarantine(self) -> QuarantinedBlockControllers {
        self.quarantined
    }
}

/// Selects and reserves only controllers fully covered by one guest.
///
/// Selection validates every controller before creating the first reservation.
/// If any later reservation fails, dropping the already-created linear permits
/// cancels them and every controller remains in the host-running phase.
pub fn prepare_runtime_controllers_for_passthrough(
    guest_regions: &[GuestPassthroughRegion],
) -> Result<PreparedBlockHandoff, BlockHandoffError> {
    if guest_regions.is_empty() {
        return Ok(PreparedBlockHandoff {
            controllers: Vec::new(),
            selected_guest_keys: Box::new([]),
            selected_pci_endpoints: Box::new([]),
        });
    }

    let controllers = runtime_handoff_controllers();
    let mut selected = Vec::new();
    for (slot, controller) in controllers {
        let slot = u32::try_from(slot)
            .map_err(|_| BlockHandoffError::InvalidState(controller.name().into()))?;
        let Some(owner) = select_controller_owner(
            controller.name(),
            controller.host_physical_ranges(),
            guest_regions,
        )?
        else {
            continue;
        };
        selected.push((
            BlockControllerIdentity::new(slot),
            controller.pci_endpoint(),
            controller,
            owner,
        ));
    }

    let selected_guest_keys = selected
        .iter()
        .map(|(_, _, _, owner)| *owner)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let selected_pci_endpoints = selected
        .iter()
        .filter_map(|(_, endpoint, ..)| *endpoint)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let mut prepared = Vec::with_capacity(selected.len());
    for (identity, _, controller, _) in selected {
        prepared.push(controller.reserve_handoff(identity)?);
    }

    Ok(PreparedBlockHandoff {
        controllers: prepared,
        selected_guest_keys,
        selected_pci_endpoints,
    })
}

#[cfg(test)]
mod tests {
    use alloc::rc::Rc;
    use core::cell::Cell;

    use super::*;

    const HOST_RUNNING: u8 = 0;
    const GUEST_OWNED: u8 = 1;
    const QUARANTINED: u8 = 2;
    const CANCELED: u8 = 3;

    struct FakePreparedController {
        identity: BlockControllerIdentity,
        fail_commit: bool,
        fail_return: bool,
        state: Rc<Cell<u8>>,
    }

    #[derive(Debug)]
    struct FakeGuestController {
        identity: BlockControllerIdentity,
        fail_return: bool,
        state: Rc<Cell<u8>>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct FakeQuarantine(BlockControllerIdentity);

    impl Drop for FakePreparedController {
        fn drop(&mut self) {
            if self.state.get() == HOST_RUNNING {
                self.state.set(CANCELED);
            }
        }
    }

    impl PreparedControllerTransaction for FakePreparedController {
        type Error = &'static str;
        type GuestOwned = FakeGuestController;
        type Quarantined = FakeQuarantine;

        fn identity(&self) -> BlockControllerIdentity {
            self.identity
        }

        fn commit(self) -> Result<Self::GuestOwned, (Self::Error, Self::Quarantined)> {
            if self.fail_commit {
                self.state.set(QUARANTINED);
                return Err(("commit", FakeQuarantine(self.identity)));
            }
            self.state.set(GUEST_OWNED);
            Ok(FakeGuestController {
                identity: self.identity,
                fail_return: self.fail_return,
                state: Rc::clone(&self.state),
            })
        }
    }

    impl GuestControllerTransaction for FakeGuestController {
        type Error = &'static str;
        type Quarantined = FakeQuarantine;

        fn quarantine(self) -> Self::Quarantined {
            self.state.set(QUARANTINED);
            FakeQuarantine(self.identity)
        }

        fn return_to_host(
            self,
        ) -> Result<BlockControllerIdentity, (Self::Error, Self::Quarantined)> {
            if self.fail_return {
                self.state.set(QUARANTINED);
                Err(("return", FakeQuarantine(self.identity)))
            } else {
                self.state.set(HOST_RUNNING);
                Ok(self.identity)
            }
        }
    }

    #[test]
    fn controller_identity_is_value_only_and_stable() {
        let identity = BlockControllerIdentity::new(7);

        assert_eq!(identity.runtime_slot(), 7);
        assert_eq!(identity.generation(), 1);
        assert_eq!(core::mem::size_of::<BlockControllerIdentity>(), 8);
    }

    #[test]
    fn partial_commit_quarantines_crossed_controllers_and_cancels_the_rest() {
        let states = core::array::from_fn::<_, 3, _>(|_| Rc::new(Cell::new(HOST_RUNNING)));
        let prepared = (0..3)
            .map(|slot| FakePreparedController {
                identity: BlockControllerIdentity::new(slot),
                fail_commit: slot == 1,
                fail_return: false,
                state: Rc::clone(&states[slot as usize]),
            })
            .collect::<Vec<_>>();

        let failure = commit_controller_batch(prepared).unwrap_err();

        assert_eq!(failure.source, "commit");
        assert_eq!(
            failure
                .quarantined
                .iter()
                .map(|controller| controller.0.runtime_slot())
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(failure.canceled, [BlockControllerIdentity::new(2)]);
        assert_eq!(
            states.map(|state| state.get()),
            [QUARANTINED, QUARANTINED, CANCELED]
        );
    }

    #[test]
    fn guest_return_finishes_other_controllers_after_one_is_quarantined() {
        let states = core::array::from_fn::<_, 3, _>(|_| Rc::new(Cell::new(GUEST_OWNED)));
        let guest_owned = (0..3)
            .map(|slot| FakeGuestController {
                identity: BlockControllerIdentity::new(slot),
                fail_return: slot == 1,
                state: Rc::clone(&states[slot as usize]),
            })
            .collect::<Vec<_>>();

        let failure = return_controller_batch(guest_owned).unwrap_err();

        assert_eq!(failure.source, "return");
        assert_eq!(
            failure
                .returned
                .iter()
                .map(|identity| identity.runtime_slot())
                .collect::<Vec<_>>(),
            [0, 2]
        );
        assert_eq!(
            failure.quarantined,
            [FakeQuarantine(BlockControllerIdentity::new(1))]
        );
        assert_eq!(
            states.map(|state| state.get()),
            [HOST_RUNNING, QUARANTINED, HOST_RUNNING]
        );
    }

    #[test]
    fn controller_selection_requires_complete_coverage_by_one_guest() {
        let resources = [
            HostPhysicalRange::new(0x1000, 0x100).unwrap(),
            HostPhysicalRange::new(0x3000, 0x80).unwrap(),
        ];
        let guest = StorageGuestKey::new(7);
        let regions = [
            GuestPassthroughRegion::new(guest, HostPhysicalRange::new(0x1000, 0x100).unwrap()),
            GuestPassthroughRegion::new(guest, HostPhysicalRange::new(0x2f00, 0x200).unwrap()),
        ];

        assert_eq!(
            select_controller_owner("nvme0", &resources, &regions).unwrap(),
            Some(guest)
        );
    }

    #[test]
    fn controller_selection_accepts_adjacent_regions_from_one_guest() {
        let resources = [HostPhysicalRange::new(0x1000, 0x100).unwrap()];
        let guest = StorageGuestKey::new(9);
        let regions = [
            GuestPassthroughRegion::new(guest, HostPhysicalRange::new(0x1000, 0x80).unwrap()),
            GuestPassthroughRegion::new(guest, HostPhysicalRange::new(0x1080, 0x80).unwrap()),
        ];

        assert_eq!(
            select_controller_owner("nvme0", &resources, &regions).unwrap(),
            Some(guest)
        );
    }

    #[test]
    fn controller_selection_ignores_disjoint_guest_mappings() {
        let resources = [HostPhysicalRange::new(0x1000, 0x100).unwrap()];
        let regions = [GuestPassthroughRegion::new(
            StorageGuestKey::new(3),
            HostPhysicalRange::new(0x3000, 0x100).unwrap(),
        )];

        assert_eq!(
            select_controller_owner("sdhci", &resources, &regions).unwrap(),
            None
        );
    }

    #[test]
    fn controller_selection_rejects_missing_resource_identity() {
        let regions = [GuestPassthroughRegion::new(
            StorageGuestKey::new(3),
            HostPhysicalRange::new(0x3000, 0x100).unwrap(),
        )];

        assert!(matches!(
            select_controller_owner("unknown", &[], &regions),
            Err(BlockHandoffError::MissingResourceIdentity { .. })
        ));
    }

    #[test]
    fn controller_selection_rejects_partial_guest_register_access() {
        let resources = [HostPhysicalRange::new(0x1000, 0x100).unwrap()];
        let regions = [GuestPassthroughRegion::new(
            StorageGuestKey::new(3),
            HostPhysicalRange::new(0x1080, 0x100).unwrap(),
        )];

        assert!(matches!(
            select_controller_owner("sdhci", &resources, &regions),
            Err(BlockHandoffError::PartialResourceCoverage { .. })
        ));
    }

    #[test]
    fn controller_selection_rejects_multiple_guest_owners() {
        let resources = [HostPhysicalRange::new(0x1000, 0x100).unwrap()];
        let regions = [
            GuestPassthroughRegion::new(
                StorageGuestKey::new(1),
                HostPhysicalRange::new(0x1000, 0x100).unwrap(),
            ),
            GuestPassthroughRegion::new(
                StorageGuestKey::new(2),
                HostPhysicalRange::new(0x1000, 0x100).unwrap(),
            ),
        ];

        assert!(matches!(
            select_controller_owner("virtio-blk", &resources, &regions),
            Err(BlockHandoffError::AmbiguousGuestOwners { .. })
        ));
    }
}
