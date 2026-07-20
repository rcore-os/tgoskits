//! Host storage ownership handoff state shared by every architecture.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    format,
    string::String,
    vec::Vec,
};

use ax_std::os::arceos::modules::ax_runtime::block::{
    BlockControllerIdentity, GuestAccessRevoked, GuestOwnedBlockControllers,
    GuestPassthroughRegion, HostPhysicalRange, HostRunningBlockControllers, PreparedBlockHandoff,
    QuarantinedBlockControllers, StorageGuestKey,
};

use crate::{AxVMRef, host::irq_routes::GuestIrqRoutesRevoked};

/// Observable phase of one host-storage ownership transfer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostStorageHandoffState {
    /// Controllers are reserved without closing host I/O admission.
    Prepared,
    /// Passthrough routing is ready and controller ownership belongs to the guest.
    GuestOwned,
    /// Controller return and filesystem remount completed.
    Returned,
    /// A destructive transition failed and affected resources remain unavailable.
    FailedClosed,
}

/// Failure while transferring host storage ownership to or from a guest.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HostStorageHandoffError {
    /// Runtime controller reservation failed without changing host admission.
    #[error("could not reserve host block controllers: {detail}")]
    ControllerPrepareRolledBack { detail: String },
    /// Filesystem freeze could not start, so ownership stayed with the host.
    #[error("could not freeze the host filesystem: {detail}")]
    Freeze { detail: String },
    /// Filesystem detach failed before destructive publication and was canceled.
    #[error("host filesystem detach failed and the freeze was rolled back: {detail}")]
    FilesystemDetachRolledBack { detail: String },
    /// Filesystem detach crossed a destructive boundary and remains unavailable.
    #[error("host filesystem detach failed closed: {detail}")]
    FilesystemDetachFailedClosed { detail: String },
    /// Controller commit crossed the destructive boundary and retained explicit owners offline.
    #[error(
        "host block-controller commit failed closed for {quarantined_controllers:?}; untouched \
         reservations {canceled_controllers:?} were canceled: {detail}"
    )]
    ControllerCommitFailedClosed {
        quarantined_controllers: Vec<BlockControllerIdentity>,
        canceled_controllers: Vec<BlockControllerIdentity>,
        detail: String,
    },
    /// Controller return completed only for the named subset.
    #[error(
        "host block-controller return restored {returned_controllers:?} and quarantined \
         {quarantined_controllers:?}: {detail}"
    )]
    ControllerReturnFailedClosed {
        returned_controllers: Vec<BlockControllerIdentity>,
        quarantined_controllers: Vec<BlockControllerIdentity>,
        detail: String,
    },
    /// Filesystem reconstruction failed after controller ownership returned.
    #[error("host filesystem remount failed closed: {detail}")]
    FilesystemRemountFailedClosed { detail: String },
    /// A stopped VM still retained an IRQ, MMIO, or vCPU access path.
    #[error("guest storage route revocation failed closed: {detail}")]
    GuestRouteRevocationFailedClosed { detail: String },
    /// VM mappings could not be converted into an exact controller selection.
    #[error("could not derive guest storage ownership: {detail}")]
    GuestSelection { detail: String },
    /// The requested handoff operation does not match the current phase.
    #[error("host storage handoff is in state {state:?}, expected {expected:?}")]
    InvalidState {
        state: HostStorageHandoffState,
        expected: HostStorageHandoffState,
    },
}

/// PCI address of one controller selected for the storage transaction.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HostStoragePciEndpoint {
    /// PCI segment group.
    pub segment: u16,
    /// PCI bus number.
    pub bus: u8,
    /// PCI device number.
    pub device: u8,
    /// PCI function number.
    pub function: u8,
}

/// Proof created only after the virtualization layer revokes guest storage routes.
#[derive(Debug)]
#[must_use = "host controller recovery requires this route-revocation proof"]
pub struct GuestStorageRoutesRevoked {
    runtime: GuestAccessRevoked,
}

pub(crate) struct StorageGuestSelection {
    guests: BTreeMap<StorageGuestKey, AxVMRef>,
    regions: Vec<GuestPassthroughRegion>,
}

impl StorageGuestSelection {
    pub(crate) fn discover() -> Result<Self, HostStorageHandoffError> {
        let mut guests = BTreeMap::new();
        let mut regions = Vec::new();
        for guest in crate::get_vm_list() {
            if !guest
                .uses_passthrough_access()
                .map_err(|error| guest_selection_error(guest.id(), error))?
            {
                continue;
            }

            let key = StorageGuestKey::new(
                u64::try_from(guest.id())
                    .map_err(|error| guest_selection_error(guest.id(), error))?,
            );
            let mappings = guest
                .passthrough_host_ranges()
                .map_err(|error| guest_selection_error(guest.id(), error))?;
            for mapping in mappings {
                let base = u64::try_from(mapping.base())
                    .map_err(|error| guest_selection_error(guest.id(), error))?;
                let length = u64::try_from(mapping.length())
                    .map_err(|error| guest_selection_error(guest.id(), error))?;
                let range = HostPhysicalRange::new(base, length)
                    .map_err(|error| guest_selection_error(guest.id(), error))?;
                regions.push(GuestPassthroughRegion::new(key, range));
            }
            if !regions.iter().any(|region| region.owner() == key) {
                continue;
            }
            if guests.insert(key, guest).is_some() {
                return Err(HostStorageHandoffError::GuestSelection {
                    detail: format!("duplicate VM storage key {key:?}"),
                });
            }
        }
        Ok(Self { guests, regions })
    }

    pub(crate) fn regions(&self) -> &[GuestPassthroughRegion] {
        &self.regions
    }

    pub(crate) fn selected_guests(
        &self,
        selected_guest_keys: impl IntoIterator<Item = StorageGuestKey>,
    ) -> Result<Box<[AxVMRef]>, HostStorageHandoffError> {
        let keys = selected_guest_keys.into_iter().collect::<BTreeSet<_>>();
        keys.into_iter()
            .map(|key| {
                self.guests.get(&key).cloned().ok_or_else(|| {
                    HostStorageHandoffError::GuestSelection {
                        detail: format!("selected block-controller owner {key:?} disappeared"),
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Vec::into_boxed_slice)
    }
}

fn guest_selection_error(vm_id: usize, error: impl core::fmt::Display) -> HostStorageHandoffError {
    HostStorageHandoffError::GuestSelection {
        detail: format!("VM[{vm_id}]: {error}"),
    }
}

impl GuestStorageRoutesRevoked {
    fn from_revoked_vms() -> Self {
        Self {
            runtime: unsafe {
                // SAFETY: `revoke_guest_storage_routes` first joins every
                // stopped vCPU runtime, drains architecture IRQ routes, and
                // removes every passthrough stage-2 mapping. The lower block
                // runtime may now reset/quiesce residual device DMA without a
                // concurrent guest control path.
                GuestAccessRevoked::new()
            },
        }
    }

    pub(crate) fn into_runtime(self) -> GuestAccessRevoked {
        self.runtime
    }
}

/// Revokes every stopped passthrough VM's access to host storage.
///
/// The transaction runs in three ordered phases: join all vCPU tasks, revoke
/// and drain architecture IRQ routes, then remove stage-2 passthrough mappings.
/// A failure returns no proof and leaves controller ownership fail-closed.
///
/// # Errors
///
/// Returns [`HostStorageHandoffError::GuestRouteRevocationFailedClosed`] when
/// there is no passthrough guest, a guest is still running, an architecture
/// route cannot be drained, or a stage-2 mapping cannot be removed.
pub fn revoke_guest_storage_routes(
    handoff: &HostStorageHandoff,
    routes_revoked: &GuestIrqRoutesRevoked,
) -> Result<GuestStorageRoutesRevoked, HostStorageHandoffError> {
    handoff.require_state(HostStorageHandoffState::GuestOwned)?;
    let guests = handoff.guests();
    if guests.is_empty() {
        return Err(route_revocation_error(
            "the storage handoff retained no guest owner",
        ));
    }

    for vm in guests {
        if !routes_revoked.covers_guest(vm) {
            return Err(route_revocation_error(format!(
                "VM[{}] was not covered by the retained IRQ-route lease",
                vm.id()
            )));
        }
    }
    for vm in guests {
        vm.revoke_passthrough_access()
            .map_err(|error| route_revocation_error(format!("VM[{}]: {error}", vm.id())))?;
    }

    Ok(GuestStorageRoutesRevoked::from_revoked_vms())
}

fn route_revocation_error(detail: impl Into<String>) -> HostStorageHandoffError {
    HostStorageHandoffError::GuestRouteRevocationFailedClosed {
        detail: detail.into(),
    }
}

enum ControllerOwnership {
    Prepared(PreparedBlockHandoff),
    GuestOwned(GuestOwnedBlockControllers),
    HostRunning(HostRunningBlockControllers),
    Quarantined(QuarantinedBlockControllers),
    Empty,
}

/// Linear record of storage resources reserved or transferred for a guest.
#[must_use = "a host-storage handoff must be returned or retained fail-closed"]
pub struct HostStorageHandoff {
    state: HostStorageHandoffState,
    controllers: ControllerOwnership,
    guests: Box<[AxVMRef]>,
    pci_endpoints: Box<[HostStoragePciEndpoint]>,
}

impl HostStorageHandoff {
    pub(crate) fn prepared(controllers: PreparedBlockHandoff, guests: Box<[AxVMRef]>) -> Self {
        let pci_endpoints = controllers
            .selected_pci_endpoints()
            .iter()
            .map(|endpoint| HostStoragePciEndpoint {
                segment: endpoint.segment(),
                bus: endpoint.bus(),
                device: endpoint.device(),
                function: endpoint.function(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            state: HostStorageHandoffState::Prepared,
            controllers: ControllerOwnership::Prepared(controllers),
            guests,
            pci_endpoints,
        }
    }

    pub(crate) fn guests(&self) -> &[AxVMRef] {
        &self.guests
    }

    /// Returns the exact selected PCI block-controller addresses.
    pub fn pci_endpoints(&self) -> &[HostStoragePciEndpoint] {
        &self.pci_endpoints
    }

    /// Returns the current fail-closed ownership phase.
    pub const fn state(&self) -> HostStorageHandoffState {
        self.state
    }

    /// Returns the controller identities retained by the current token.
    pub fn controller_identities(&self) -> Vec<BlockControllerIdentity> {
        match &self.controllers {
            ControllerOwnership::Prepared(controllers) => controllers.identities().collect(),
            ControllerOwnership::GuestOwned(controllers) => controllers.identities().collect(),
            ControllerOwnership::HostRunning(controllers) => controllers.identities().collect(),
            ControllerOwnership::Quarantined(controllers) => controllers.identities().collect(),
            ControllerOwnership::Empty => Vec::new(),
        }
    }

    pub(crate) fn commit_to_guest(&mut self) -> Result<(), HostStorageHandoffError> {
        self.require_state(HostStorageHandoffState::Prepared)?;
        let prepared = match core::mem::replace(&mut self.controllers, ControllerOwnership::Empty) {
            ControllerOwnership::Prepared(prepared) => prepared,
            ownership => {
                self.controllers = ownership;
                return Err(self.invalid_state(HostStorageHandoffState::Prepared));
            }
        };
        match prepared.commit() {
            Ok(guest_owned) => {
                self.controllers = ControllerOwnership::GuestOwned(guest_owned);
                self.state = HostStorageHandoffState::GuestOwned;
                Ok(())
            }
            Err(failure) => {
                let quarantined_controllers = failure.quarantined().identities().collect();
                let canceled_controllers = failure.canceled_identities().to_vec();
                let detail = format!("{}", failure.source_error());
                self.controllers = ControllerOwnership::Quarantined(failure.into_quarantine());
                self.state = HostStorageHandoffState::FailedClosed;
                Err(HostStorageHandoffError::ControllerCommitFailedClosed {
                    quarantined_controllers,
                    canceled_controllers,
                    detail,
                })
            }
        }
    }

    pub(crate) fn cancel_prepared(&mut self) -> Result<(), HostStorageHandoffError> {
        self.require_state(HostStorageHandoffState::Prepared)?;
        let prepared = match core::mem::replace(&mut self.controllers, ControllerOwnership::Empty) {
            ControllerOwnership::Prepared(prepared) => prepared,
            ownership => {
                self.controllers = ownership;
                return Err(self.invalid_state(HostStorageHandoffState::Prepared));
            }
        };
        drop(prepared);
        Ok(())
    }

    pub(crate) fn return_controllers(
        &mut self,
        revoked: GuestStorageRoutesRevoked,
    ) -> Result<(), HostStorageHandoffError> {
        self.require_state(HostStorageHandoffState::GuestOwned)?;
        let guest_owned =
            match core::mem::replace(&mut self.controllers, ControllerOwnership::Empty) {
                ControllerOwnership::GuestOwned(guest_owned) => guest_owned,
                ownership => {
                    self.controllers = ownership;
                    return Err(self.invalid_state(HostStorageHandoffState::GuestOwned));
                }
            };
        match guest_owned.return_to_host(revoked.into_runtime()) {
            Ok(host_running) => {
                self.controllers = ControllerOwnership::HostRunning(host_running);
                Ok(())
            }
            Err(failure) => {
                let returned_controllers = failure.returned_identities().to_vec();
                let quarantined_controllers = failure.quarantined().identities().collect();
                let detail = format!("{}", failure.source_error());
                self.controllers = ControllerOwnership::Quarantined(failure.into_quarantine());
                self.state = HostStorageHandoffState::FailedClosed;
                Err(HostStorageHandoffError::ControllerReturnFailedClosed {
                    returned_controllers,
                    quarantined_controllers,
                    detail,
                })
            }
        }
    }

    pub(crate) fn complete_return(&mut self) {
        self.controllers = ControllerOwnership::Empty;
        self.guests = Vec::new().into_boxed_slice();
        self.pci_endpoints = Vec::new().into_boxed_slice();
        self.state = HostStorageHandoffState::Returned;
    }

    pub(crate) fn mark_failed_closed(&mut self) {
        self.state = HostStorageHandoffState::FailedClosed;
    }

    fn require_state(
        &self,
        expected: HostStorageHandoffState,
    ) -> Result<(), HostStorageHandoffError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(self.invalid_state(expected))
        }
    }

    const fn invalid_state(&self, expected: HostStorageHandoffState) -> HostStorageHandoffError {
        HostStorageHandoffError::InvalidState {
            state: self.state,
            expected,
        }
    }
}

impl core::fmt::Debug for HostStorageHandoff {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("HostStorageHandoff")
            .field("state", &self.state)
            .field("controllers", &self.controller_identities())
            .field(
                "guests",
                &self
                    .guests
                    .iter()
                    .map(|guest| guest.id())
                    .collect::<Vec<_>>(),
            )
            .field("pci_endpoints", &self.pci_endpoints)
            .finish()
    }
}
