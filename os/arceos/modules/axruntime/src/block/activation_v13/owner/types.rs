//! Publication catalog and exact owners retained across activation failure.

use super::*;

/// Published v0.13 catalog whose hardware owners remain on pinned threads.
#[must_use = "retain the installation while its controller is published"]
pub struct ReadyControllerInstallation {
    pub(super) name: String,
    pub(super) logical_devices: Box<[LogicalDeviceDesc]>,
    pub(super) routes: Box<[LogicalDeviceRoute]>,
    pub(super) domains: Box<[BoundDomainDesc]>,
    pub(super) owner_cpu: usize,
    pub(super) owner_thread: crate::task::ThreadId,
    pub(super) devices: Box<[V13BlockDeviceView]>,
    pub(super) owners: ReadyControllerOwners,
}

impl ReadyControllerInstallation {
    /// Returns the portable controller name captured during discovery.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns immutable geometry published only after every domain proof.
    pub fn logical_devices(&self) -> &[LogicalDeviceDesc] {
        &self.logical_devices
    }

    /// Returns immutable logical-device to hardware-queue routing.
    pub fn routes(&self) -> &[LogicalDeviceRoute] {
        &self.routes
    }

    /// Returns final fixed-owner facts for every hardware ownership domain.
    pub fn domains(&self) -> &[BoundDomainDesc] {
        &self.domains
    }

    /// Returns synchronous facades backed by immutable per-CPU hctx maps.
    pub fn logical_device_views(&self) -> &[V13BlockDeviceView] {
        &self.devices
    }

    /// Returns the CPU that owns controller control and shared IRQ state.
    pub const fn owner_cpu(&self) -> usize {
        self.owner_cpu
    }

    /// Returns the generation-bearing controller maintenance thread identity.
    pub const fn owner_thread(&self) -> crate::task::ThreadId {
        self.owner_thread
    }

    /// Closes every fixed maintenance owner as one linear controller transaction.
    ///
    /// The control owner freezes admission and coordinates IRQ/DMA teardown;
    /// this caller only requests that transaction and waits until every owner
    /// has published its terminal maintenance close proof. Any failure returns
    /// the complete installation so it can be retried or quarantined intact.
    pub fn close(self) -> Result<(), ReadyControllerCloseFailure> {
        if let Err(error) = self.owners.control_remote.request_shutdown() {
            return Err(ReadyControllerCloseFailure::new(
                "request controller shutdown",
                ReadyControllerCloseError::Maintenance(error),
                self,
            ));
        }
        if let Err(error) = self.owners.control_thread.wait() {
            return Err(ReadyControllerCloseFailure::new(
                "wait for controller maintenance owner",
                ReadyControllerCloseError::Task(error),
                self,
            ));
        }
        for child in &self.owners.child_domains {
            if let Err(error) = child.thread.wait() {
                return Err(ReadyControllerCloseFailure::new(
                    "wait for I/O-domain maintenance owner",
                    ReadyControllerCloseError::Task(error),
                    self,
                ));
            }
        }
        Ok(())
    }
}

pub(super) struct ReadyControllerOwners {
    pub(super) control_thread: MaintenanceThread,
    pub(super) control_remote: DeviceMaintenanceHandle<V13MaintenanceEvent>,
    pub(super) child_domains: Box<[InstalledChildDomain]>,
    pub(super) _topology: Arc<FixedOwnershipTopology>,
}

pub(super) struct InstalledChildDomain {
    pub(super) remote: DeviceMaintenanceHandle<V13MaintenanceEvent>,
    pub(super) thread: MaintenanceThread,
    pub(super) requests: Arc<DomainRequestRuntime>,
    pub(super) reinit: Arc<super::super::reinit::DomainReinitPermitCell>,
}

pub(super) struct ControlOwnerReady {
    pub(super) name: String,
    pub(super) logical_devices: Box<[LogicalDeviceDesc]>,
    pub(super) routes: Box<[LogicalDeviceRoute]>,
    pub(super) domains: Box<[BoundDomainDesc]>,
    pub(super) control_remote: DeviceMaintenanceHandle<V13MaintenanceEvent>,
    pub(super) devices: Box<[V13BlockDeviceView]>,
    pub(super) child_domains: Box<[InstalledChildDomain]>,
    pub(super) topology: Arc<FixedOwnershipTopology>,
}

impl ControlOwnerReady {
    pub(super) fn from_published(
        controller_name: &str,
        published: &RdifBlockPublishedOwner,
        control_remote: DeviceMaintenanceHandle<V13MaintenanceEvent>,
        devices: Box<[V13BlockDeviceView]>,
        child_domains: Vec<InstalledChildDomain>,
        topology: Arc<FixedOwnershipTopology>,
    ) -> Self {
        Self {
            name: String::from(controller_name),
            logical_devices: published
                .published()
                .logical_devices()
                .to_vec()
                .into_boxed_slice(),
            routes: published
                .published()
                .logical_device_routes()
                .to_vec()
                .into_boxed_slice(),
            domains: published
                .published()
                .bound_domains()
                .to_vec()
                .into_boxed_slice(),
            control_remote,
            devices,
            child_domains: child_domains.into_boxed_slice(),
            topology,
        }
    }

    pub(super) fn into_installation(
        self,
        control_thread: MaintenanceThread,
    ) -> ReadyControllerInstallation {
        let owner_cpu = self.control_remote.owner_cpu();
        let owner_thread = self.control_remote.owner_thread();
        ReadyControllerInstallation {
            name: self.name,
            logical_devices: self.logical_devices,
            routes: self.routes,
            domains: self.domains,
            owner_cpu,
            owner_thread,
            devices: self.devices,
            owners: ReadyControllerOwners {
                control_thread,
                control_remote: self.control_remote,
                child_domains: self.child_domains,
                _topology: self.topology,
            },
        }
    }
}

pub(super) struct FinalDomainInstallation {
    pub(super) shared_domain: Option<InstalledSharedDomain>,
    pub(super) child_domains: Vec<InstalledChildDomain>,
}

pub(super) struct InstalledSharedDomain {
    pub(super) domain: InstalledIoDomain,
    pub(super) requests: DomainRequestOwner,
}

pub(super) struct PreparedEnableFailure {
    pub(super) phase: &'static str,
    pub(super) owner: PreparedEnableOwner,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(super) enum RequestRuntimeInstallError {
    #[error(transparent)]
    Build(RequestRuntimeBuildError),
    #[error(transparent)]
    Maintenance(crate::maintenance::MaintenanceSubmitError),
    #[error("published controller exposes both split and combined shared I/O owners")]
    ConflictingSharedOwner,
}

pub(super) enum PreparedEnableOwner {
    Maintenance {
        _error: crate::maintenance::MaintenanceError,
    },
    Driver {
        _error: rdif_block::BlkError,
    },
    Binding {
        _error: ax_driver::IrqBindingError,
    },
}

pub(super) struct FinalDomainInstallFailure {
    pub(super) phase: &'static str,
    // Domains installed before the failing step already own live threads,
    // actions, request tables, and portable driver parts. Keep the complete
    // partial transaction in the control-thread quarantine owner.
    pub(super) previous: Option<Box<FinalDomainInstallation>>,
    pub(super) _retained: Box<FinalDomainRetained>,
}

pub(super) enum FinalDomainRetained {
    Unbound {
        _domain: Box<UnboundIoDomain>,
    },
    UnboundWithSources {
        _domain: Box<UnboundIoDomain>,
        _platform_sources: Box<super::super::domain::DomainPlatformSources>,
    },
    Maintenance {
        _domain: Box<UnboundIoDomain>,
        _platform_sources: Box<super::super::domain::DomainPlatformSources>,
        _error: crate::maintenance::MaintenanceSubmitError,
    },
    SourceRegistration {
        _failure: Box<SourceRegistrationFailure>,
        _platform_sources: Box<super::super::domain::DomainPlatformSources>,
    },
    SourceEnable {
        _source: Box<BoundEvidenceSource>,
        _platform_sources: Box<super::super::domain::DomainPlatformSources>,
        _error: crate::maintenance::MaintenanceError,
    },
    Install {
        _failure: Box<rdif_block::DomainInstallFailure>,
    },
    Spawn {
        _owner: Box<RetainedDomainSpawnOwner>,
    },
    RequestRuntime {
        _domain: Box<UnboundIoDomain>,
        _platform_sources: Box<super::super::domain::DomainPlatformSources>,
        _error: RequestRuntimeBuildError,
    },
    PlatformSourceTransfer {
        _domain: Box<UnboundIoDomain>,
        _platform_sources: Box<super::super::domain::DomainPlatformSources>,
        _error: ax_driver::ExactIrqSourceBindingError,
    },
    UnmatchedPlatformSources {
        _domain: Box<UnboundIoDomain>,
        _platform_sources: Box<super::super::domain::DomainPlatformSources>,
    },
    OwnerIdentity,
    CombinedBinding {
        _error: rdif_block::ActivationError,
    },
    SharedProof {
        _domain: Box<InstalledSharedDomain>,
        _proof: Box<rdif_block::BoundDomainProofFailure>,
    },
    ChildProof {
        _remote: DeviceMaintenanceHandle<V13MaintenanceEvent>,
        _thread: MaintenanceThread,
        _requests: Arc<DomainRequestRuntime>,
        _reinit: Arc<super::super::reinit::DomainReinitPermitCell>,
        _proof: Box<rdif_block::BoundDomainProofFailure>,
    },
}

pub(super) struct ReadyServiceFailure {
    pub(super) phase: &'static str,
    pub(super) retained: ReadyServiceRetained,
}

pub(super) enum ReadyServiceRetained {
    Io {
        _pending: rdif_block::PendingBlockIrq,
        _error: rdif_block::BlkError,
    },
    Control {
        _failure: Box<rdif_block::ReadyEvidenceServiceFailure>,
    },
    Requests {
        _pending: rdif_block::PendingBlockIrq,
        _error: super::super::request_runtime::DomainRequestServiceError,
    },
    CombinedUnavailable {
        _pending: rdif_block::PendingBlockIrq,
    },
}

pub(super) enum ControlOwnerLaunchFailure {
    Selection(Box<ControllerSelectionFailure>),
    Unspawned {
        phase: &'static str,
        _retained: Box<SelectedControllerActivation>,
    },
    Running {
        phase: &'static str,
        _thread: MaintenanceThread,
    },
}

impl ControlOwnerLaunchFailure {
    pub(super) fn phase(&self) -> &'static str {
        match self {
            Self::Selection(failure) => match failure.error() {
                V13SelectionErrorRef::Activation => "select controller activation",
                V13SelectionErrorRef::SnapshotChanged => "validate capability snapshot",
            },
            Self::Unspawned { phase, .. } | Self::Running { phase, .. } => phase,
        }
    }
}

pub(super) struct ControlOwnerStartupFailure {
    pub(super) phase: &'static str,
}

/// Typed reason why an explicit published-controller close could not finish.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReadyControllerCloseError {
    /// The control owner rejected or could not make progress on shutdown.
    #[error(transparent)]
    Maintenance(crate::maintenance::MaintenanceSubmitError),
    /// A fixed maintenance owner could not be observed in terminal state.
    #[error(transparent)]
    Task(crate::task::TaskError),
}

/// Failed controller close retaining every published facade and owner handle.
#[must_use = "retry close or retain the complete controller in named quarantine"]
pub struct ReadyControllerCloseFailure {
    phase: &'static str,
    error: ReadyControllerCloseError,
    retained: Box<ReadyControllerInstallation>,
}

impl ReadyControllerCloseFailure {
    fn new(
        phase: &'static str,
        error: ReadyControllerCloseError,
        retained: ReadyControllerInstallation,
    ) -> Self {
        Self {
            phase,
            error,
            retained: Box::new(retained),
        }
    }

    /// Returns the close phase that failed.
    pub const fn phase(&self) -> &'static str {
        self.phase
    }

    /// Borrows the typed close failure.
    pub const fn error(&self) -> &ReadyControllerCloseError {
        &self.error
    }

    /// Returns the failure and the unchanged published-controller owner.
    pub fn into_parts(self) -> (ReadyControllerCloseError, ReadyControllerInstallation) {
        (self.error, *self.retained)
    }
}

impl core::fmt::Debug for ReadyControllerCloseFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ReadyControllerCloseFailure")
            .field("phase", &self.phase)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl core::fmt::Display for ReadyControllerCloseFailure {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            formatter,
            "published controller close failed during {}: {}",
            self.phase, self.error
        )
    }
}

impl core::error::Error for ReadyControllerCloseFailure {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_controller_close_is_linear_and_owner_retaining() {
        let _close: fn(ReadyControllerInstallation) -> Result<(), ReadyControllerCloseFailure> =
            ReadyControllerInstallation::close;
    }
}
