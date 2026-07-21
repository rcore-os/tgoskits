//! Binding one portable evidence source to its final IRQ owner.

use alloc::{boxed::Box, format, sync::Arc};

use ax_hal::irq::{IrqContext, IrqReturn};
use rdif_block::{BIrqControl, BlockEvidenceSource, DomainIrqSource, IrqSourceId};

use super::{
    super::{FixedDomainOwner, V13ActivationError},
    BoundEvidenceSource, EndpointCallbackCell, EvidenceIngress, V13MaintenanceEvent,
    source_irq_action,
};
use crate::maintenance::{
    LocalIrqWake, MaintenanceError, MaintenanceIrqAction, MaintenanceRegistrar, MaintenanceSession,
};

impl BoundEvidenceSource {
    /// Registers one endpoint disabled on its final maintenance owner.
    pub(in crate::block::activation_v13) fn register_control_disabled(
        controller_name: &str,
        registrar: &MaintenanceRegistrar<V13MaintenanceEvent>,
        owner: &FixedDomainOwner,
        portable: &mut DomainIrqSource,
        platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    ) -> Result<Self, SourceRegistrationFailure> {
        Self::register_disabled(controller_name, registrar, owner, portable, platform_source)
    }

    /// Registers a source discovered after init on the same live owner.
    pub(in crate::block::activation_v13) fn register_live_disabled(
        controller_name: &str,
        session: &MaintenanceSession<V13MaintenanceEvent>,
        owner: &FixedDomainOwner,
        portable: &mut DomainIrqSource,
        platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    ) -> Result<Self, SourceRegistrationFailure> {
        Self::register_disabled(controller_name, session, owner, portable, platform_source)
    }

    fn register_disabled<R: EvidenceSourceRegistrar>(
        controller_name: &str,
        registrar: &R,
        owner: &FixedDomainOwner,
        portable: &mut DomainIrqSource,
        platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    ) -> Result<Self, SourceRegistrationFailure> {
        let source = portable.id();
        let Some(topology_irq) = owner.irq_for_source(source) else {
            return Err(SourceRegistrationFailure::Configuration {
                _error: Box::new(V13ActivationError::SourceOutsideDomain {
                    domain: owner.domain(),
                    source_id: source,
                }),
                _platform_source: platform_source,
            });
        };
        if platform_source
            .as_ref()
            .is_some_and(|platform_source| platform_source.source_id() != source.get())
        {
            return Err(SourceRegistrationFailure::PlatformSourceMismatch {
                _source: source,
                _platform_source: platform_source
                    .expect("the mismatch predicate observed an exact source"),
            });
        }
        let irq = if let Some(binding_irq) = platform_source
            .as_ref()
            .map(|platform_source| platform_source.irq().clone())
        {
            let resolved = match crate::irq::resolve_binding_irq(binding_irq) {
                Ok(resolved) => resolved,
                Err(error) => {
                    return Err(SourceRegistrationFailure::PlatformSourceResolution {
                        _error: error,
                        _platform_source: platform_source
                            .expect("the route came from an exact source"),
                    });
                }
            };
            if resolved != topology_irq {
                return Err(SourceRegistrationFailure::PlatformSourceRouteChanged {
                    _topology_irq: topology_irq,
                    _resolved_irq: resolved,
                    _platform_source: platform_source.expect("the route came from an exact source"),
                });
            }
            resolved
        } else {
            topology_irq
        };
        let wake = match registrar.local_irq_wake() {
            Ok(wake) => wake,
            Err(error) => {
                return Err(SourceRegistrationFailure::Maintenance {
                    _error: error,
                    _platform_source: platform_source,
                });
            }
        };
        let portable_source = match portable.take_for_registration() {
            Ok(source) => source,
            Err(error) => {
                return Err(SourceRegistrationFailure::Configuration {
                    _error: Box::new(error.into()),
                    _platform_source: platform_source,
                });
            }
        };
        let (endpoint, control) = portable_source.into_parts();
        let ingress = Arc::new(EvidenceIngress::new(source));
        let callback = Arc::new(EndpointCallbackCell::new(endpoint, Arc::clone(&ingress)));
        let callback_for_action = Arc::clone(&callback);
        let owner_cpu = registrar.owner_cpu();
        let mut source_epoch = 0_u64;
        let action = match registrar.register_disabled(
            format!("{controller_name}/v13-source-{}", source.get()),
            irq,
            move |context| {
                source_irq_action(
                    context,
                    owner_cpu,
                    &wake,
                    &callback_for_action,
                    &mut source_epoch,
                )
            },
        ) {
            Ok(action) => action,
            Err(error) => {
                let callback = match Arc::try_unwrap(callback) {
                    Ok(callback) => callback,
                    Err(callback) => {
                        return Err(SourceRegistrationFailure::CallbackOwnerRetained {
                            _error: error,
                            _callback: callback,
                            _control: control,
                            _platform_source: platform_source,
                        });
                    }
                };
                let endpoint = callback.into_endpoint();
                let source_owner = BlockEvidenceSource::new(endpoint, control);
                if let Err((binding_error, source_owner)) =
                    portable.restore_failed_registration(source_owner)
                {
                    return Err(SourceRegistrationFailure::RestoreOwnerRetained {
                        _registration_error: error,
                        _binding_error: binding_error,
                        _source_owner: Box::new(source_owner),
                        _platform_source: platform_source,
                    });
                }
                return Err(SourceRegistrationFailure::Maintenance {
                    _error: error,
                    _platform_source: platform_source,
                });
            }
        };
        if let Err(error) = portable.finish_registration() {
            return Err(SourceRegistrationFailure::BoundOwner {
                _error: Box::new(error.into()),
                _source: Box::new(Self {
                    source,
                    ingress,
                    control,
                    action,
                    _platform_source: platform_source,
                    retained_pending: None,
                    recovery: None,
                    drain: super::SourceDrainState::Idle,
                }),
            });
        }
        Ok(Self {
            source,
            ingress,
            control,
            action,
            _platform_source: platform_source,
            retained_pending: None,
            recovery: None,
            drain: super::SourceDrainState::Idle,
        })
    }
}

trait EvidenceSourceRegistrar {
    fn owner_cpu(&self) -> usize;

    fn local_irq_wake(&self) -> Result<LocalIrqWake<V13MaintenanceEvent>, MaintenanceError>;

    fn register_disabled(
        &self,
        name: alloc::string::String,
        irq: ax_hal::irq::IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<MaintenanceIrqAction, MaintenanceError>;
}

impl EvidenceSourceRegistrar for MaintenanceRegistrar<V13MaintenanceEvent> {
    fn owner_cpu(&self) -> usize {
        MaintenanceRegistrar::owner_cpu(self)
    }

    fn local_irq_wake(&self) -> Result<LocalIrqWake<V13MaintenanceEvent>, MaintenanceError> {
        MaintenanceRegistrar::local_irq_wake(self)
    }

    fn register_disabled(
        &self,
        name: alloc::string::String,
        irq: ax_hal::irq::IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<MaintenanceIrqAction, MaintenanceError> {
        MaintenanceRegistrar::register_shared_disabled(self, name, irq, handler)
    }
}

impl EvidenceSourceRegistrar for MaintenanceSession<V13MaintenanceEvent> {
    fn owner_cpu(&self) -> usize {
        MaintenanceSession::owner_cpu(self)
    }

    fn local_irq_wake(&self) -> Result<LocalIrqWake<V13MaintenanceEvent>, MaintenanceError> {
        MaintenanceSession::local_irq_wake(self)
    }

    fn register_disabled(
        &self,
        name: alloc::string::String,
        irq: ax_hal::irq::IrqId,
        handler: impl FnMut(IrqContext) -> IrqReturn + Send + 'static,
    ) -> Result<MaintenanceIrqAction, MaintenanceError> {
        MaintenanceSession::register_shared_disabled(self, name, irq, handler)
    }
}

/// Registration failure that either restored the portable owner or retains a
/// disabled action for named quarantine.
pub(in crate::block::activation_v13) enum SourceRegistrationFailure {
    PlatformSourceTransfer {
        _error: ax_driver::ExactIrqSourceBindingError,
    },
    Configuration {
        _error: Box<V13ActivationError>,
        _platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    },
    Maintenance {
        _error: MaintenanceError,
        _platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    },
    PlatformSourceResolution {
        _error: ax_hal::irq::IrqError,
        _platform_source: ax_driver::ExactIrqSourceBinding,
    },
    PlatformSourceMismatch {
        _source: IrqSourceId,
        _platform_source: ax_driver::ExactIrqSourceBinding,
    },
    PlatformSourceRouteChanged {
        _topology_irq: ax_hal::irq::IrqId,
        _resolved_irq: ax_hal::irq::IrqId,
        _platform_source: ax_driver::ExactIrqSourceBinding,
    },
    CallbackOwnerRetained {
        _error: MaintenanceError,
        _callback: Arc<EndpointCallbackCell>,
        _control: BIrqControl,
        _platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    },
    RestoreOwnerRetained {
        _registration_error: MaintenanceError,
        _binding_error: rdif_block::IrqSourceBindingError,
        _source_owner: Box<BlockEvidenceSource>,
        _platform_source: Option<ax_driver::ExactIrqSourceBinding>,
    },
    BoundOwner {
        _error: Box<V13ActivationError>,
        _source: Box<BoundEvidenceSource>,
    },
}
