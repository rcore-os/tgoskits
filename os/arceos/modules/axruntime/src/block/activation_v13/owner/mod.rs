//! Discovery-to-publication orchestration on final maintenance owners.

use alloc::{boxed::Box, format, string::String, sync::Arc, vec::Vec};

use ax_driver::block::{RdifBlockPreparedOwner, RdifBlockPublishedOwner};
use rdif_block::{
    BoundDomainDesc, ControlDomainActivation, DmaQuiesced, EvidenceServiceResult,
    InstalledIoDomain, IoDomainIrqSource, IrqServiceDecision, LogicalDeviceDesc,
    LogicalDeviceRoute, OwnershipDomainId, UnboundIoDomain,
};

use super::{
    domain::{
        DomainPlatformSources, RetainedDomainSpawnOwner, domain_owner_binding, spawn_domain_owner,
        take_first_owner,
    },
    domain_evidence::{DomainDecisionApplied, apply_domain_decision},
    initialization::drive_controller_initialization,
    reinit::DomainReinitPermitCell,
    request_runtime::{
        DomainRequestLifecycleError, DomainRequestOwner, DomainRequestRuntime,
        DomainRequestServiceError, DomainSubmitEndpoint, RequestRuntimeBuildError,
        V13BlockDeviceView, build_published_devices,
    },
    selection::{
        ControllerSelectionFailure, SelectedControllerActivation, V13SelectionErrorRef,
        select_controller_activation,
    },
    shutdown::{ControllerShutdown, ParticipantId, ShutdownGeneration, ShutdownPhase},
    source::{BoundEvidenceSource, SourceRegistrationFailure, V13MaintenanceEvent},
    startup::{OwnerStartupCell, OwnerTransferCell},
    topology::FixedOwnershipTopology,
};
use crate::maintenance::{
    DeviceMaintenanceHandle, MaintenanceCauses, MaintenanceClosed, MaintenanceRegistrar,
    MaintenanceSession, MaintenanceThread, spawn_maintenance_domain,
};

const OWNER_SERVICE_BUDGET: usize = 64;

mod control_close;
mod evidence_service;
mod lifecycle;
mod quarantine;
mod service;
#[cfg(test)]
mod tests;
mod types;

use evidence_service::{ReadyEvidenceRoute, service_ready_evidence};
use quarantine::*;
use service::{
    ControlIoRuntime, ControlServiceOwner, RequestRuntimeInstallation, install_request_runtime,
    service_control_forever,
};
use types::{
    ControlOwnerLaunchFailure, ControlOwnerReady, ControlOwnerStartupFailure,
    FinalDomainInstallFailure, FinalDomainInstallation, FinalDomainRetained, InstalledChildDomain,
    InstalledSharedDomain, PreparedEnableFailure, PreparedEnableOwner, ReadyServiceFailure,
    ReadyServiceRetained, RequestRuntimeInstallError,
};
pub use types::{ReadyControllerCloseFailure, ReadyControllerInstallation};

/// Takes and activates every v0.13 controller registered by ax-driver.
///
/// A failed controller is never partially published. Failures after an IRQ
/// action becomes live park the exact owner thread in named quarantine; an
/// unstarted discovery transaction remains ordinary RAII-owned input.
pub fn activate_discovered_controllers_v13() -> Vec<ReadyControllerInstallation> {
    let online_cpu_count = crate::runtime_cpu_count();
    ax_driver::block::take_rdif_block_activators()
        .into_iter()
        .filter_map(
            |activator| match activate_controller(activator, online_cpu_count) {
                Ok(ready) => {
                    info!("activated rdif-block v0.13 controller {}", ready.name());
                    Some(ready)
                }
                Err(failure) => {
                    error!(
                        "rdif-block v0.13 controller activation failed closed during {}",
                        failure.phase()
                    );
                    None
                }
            },
        )
        .collect()
}

fn activate_controller(
    activator: ax_driver::block::RdifBlockActivator,
    online_cpu_count: usize,
) -> Result<ReadyControllerInstallation, ControlOwnerLaunchFailure> {
    let selected = select_controller_activation(activator, online_cpu_count)
        .map_err(|failure| ControlOwnerLaunchFailure::Selection(Box::new(failure)))?;
    spawn_control_owner(selected)
}

fn spawn_control_owner(
    selected: SelectedControllerActivation,
) -> Result<ReadyControllerInstallation, ControlOwnerLaunchFailure> {
    let control_domain = selected.plan.control_domain();
    let Some(owner) = selected.topology.domain(control_domain) else {
        return Err(ControlOwnerLaunchFailure::Unspawned {
            phase: "resolve control ownership domain",
            _retained: Box::new(selected),
        });
    };
    let owner_cpu = owner.owner_cpu();
    let controller_name = String::from(selected.parts.name());
    let thread_name = format!("blk-v13/{controller_name}/control");
    let startup = Arc::new(OwnerStartupCell::new());
    let transfer = Arc::new(OwnerTransferCell::new(selected));
    let child_startup = Arc::clone(&startup);
    let child_transfer = Arc::clone(&transfer);
    let thread = match spawn_maintenance_domain::<V13MaintenanceEvent, _>(
        owner_cpu,
        thread_name,
        move |registrar| {
            let Some(selected) = child_transfer.take() else {
                return quarantine_failed_control_registration(
                    registrar,
                    child_startup,
                    "control transfer owner missing",
                    (),
                );
            };
            run_control_owner(selected, registrar, child_startup)
        },
    ) {
        Ok(thread) => thread,
        Err(_) => {
            let retained = transfer
                .take()
                .expect("a failed spawn cannot consume the controller owner");
            return Err(ControlOwnerLaunchFailure::Unspawned {
                phase: "spawn control maintenance owner",
                _retained: Box::new(retained),
            });
        }
    };
    let result = match startup.wait_take() {
        Ok(result) => result,
        Err(_) => {
            return Err(ControlOwnerLaunchFailure::Running {
                phase: "wait for control owner publication",
                _thread: thread,
            });
        }
    };
    match result {
        Ok(ready) => Ok(ready.into_installation(thread)),
        Err(failure) => Err(ControlOwnerLaunchFailure::Running {
            phase: failure.phase,
            _thread: thread,
        }),
    }
}

fn run_control_owner(
    selected: SelectedControllerActivation,
    registrar: MaintenanceRegistrar<V13MaintenanceEvent>,
    startup: Arc<OwnerStartupCell<Result<ControlOwnerReady, ControlOwnerStartupFailure>>>,
) -> Result<crate::maintenance::MaintenanceClosed, crate::maintenance::MaintenanceError> {
    let SelectedControllerActivation {
        parts,
        plan,
        topology,
    } = selected;
    let controller_name = String::from(parts.name());
    let control_activation = plan.control_activation();
    let mut prepared = match parts.activate(plan) {
        Ok(prepared) => prepared,
        Err(failure) => {
            return quarantine_failed_control_registration(
                registrar,
                startup,
                "apply immutable activation plan",
                (failure, topology),
            );
        }
    };
    let control_domain = prepared.prepared_mut().control_mut().control_domain();
    let owner = topology
        .domain(control_domain)
        .expect("selection reserved the controller control domain");
    let control_source_ids = prepared
        .prepared_mut()
        .control_mut()
        .owned_irq_sources()
        .collect::<Vec<_>>();
    let mut sources = Vec::new();
    for source_id in control_source_ids {
        let platform_source = match prepared.take_exact_irq_source(source_id.get()) {
            Ok(platform_source) => platform_source,
            Err(error) => {
                return quarantine_failed_control_registration(
                    registrar,
                    startup,
                    "transfer exact control IRQ source",
                    (
                        prepared,
                        sources,
                        SourceRegistrationFailure::PlatformSourceTransfer { _error: error },
                        topology,
                    ),
                );
            }
        };
        let portable = prepared
            .prepared_mut()
            .control_mut()
            .owned_irq_sources_mut()
            .iter_mut()
            .find(|portable| portable.id() == source_id)
            .expect("the immutable source list came from this control owner");
        match BoundEvidenceSource::register_control_disabled(
            &controller_name,
            &registrar,
            owner,
            portable,
            platform_source,
        ) {
            Ok(source) => sources.push(source),
            Err(failure) => {
                return quarantine_failed_control_registration(
                    registrar,
                    startup,
                    "register control IRQ source",
                    (prepared, sources, failure, topology),
                );
            }
        }
    }
    let control_remote = registrar.remote_handle();
    let session = registrar.activate()?;
    if let Err(failure) = enable_prepared_controller(&mut prepared, &sources) {
        quarantine_failed_control_session(
            session,
            startup,
            failure.phase,
            (prepared, sources, failure.owner, topology),
        );
    }
    let ready = match drive_controller_initialization(&mut prepared, &session, &mut sources) {
        Ok(ready) => ready,
        Err(failure) => quarantine_failed_control_session(
            session,
            startup,
            failure.phase(),
            (prepared, sources, failure, topology),
        ),
    };
    let staged = match prepared.stage(ready) {
        Ok(staged) => staged,
        Err(failure) => quarantine_failed_control_session(
            session,
            startup,
            "stage controller publication",
            (failure, sources, topology),
        ),
    };
    let (mut publication, domains) = staged.into_installations();
    let participant_count = 1 + domains
        .iter()
        .filter(|domain| {
            domain_placement(control_activation, domain.domain_id())
                == DomainPlacement::IndependentOwner
        })
        .count();
    let shutdown_generation = ShutdownGeneration::new(session.owner_thread().as_u64())
        .expect("a live maintenance owner has a nonzero generation-bearing identity");
    let shutdown = match ControllerShutdown::new(shutdown_generation, participant_count) {
        Ok(shutdown) => Arc::new(shutdown),
        Err(error) => quarantine_failed_control_session(
            session,
            startup,
            "construct controller shutdown topology",
            (publication, domains, sources, error, topology),
        ),
    };
    let control_participant = shutdown
        .participant(0)
        .expect("controller shutdown topology always contains participant zero");
    let installation = match install_final_domains(
        domains,
        FinalDomainInstallContext {
            controller_name: &controller_name,
            control_activation,
            publication: &mut publication,
            session: &session,
            control_remote: &control_remote,
            sources: &mut sources,
            topology: &topology,
            shutdown: &shutdown,
        },
    ) {
        Ok(installation) => installation,
        Err(failure) => quarantine_failed_control_session(
            session,
            startup,
            failure.phase,
            (publication, sources, failure, topology),
        ),
    };
    let published = match publication.publish() {
        Ok(published) => published,
        Err(failure) => quarantine_failed_control_session(
            session,
            startup,
            "publish controller catalog",
            (failure, sources, installation, topology),
        ),
    };
    let request_installation = match install_request_runtime(
        &published,
        &installation,
        &control_remote,
        crate::block::BlockRuntimeConfig::default(),
    ) {
        Ok(installation) => installation,
        Err(error) => quarantine_failed_control_session(
            session,
            startup,
            "install v0.13 request runtime",
            (published, sources, installation, error, topology),
        ),
    };
    let RequestRuntimeInstallation {
        devices,
        combined_requests,
    } = request_installation;
    let FinalDomainInstallation {
        shared_domain,
        child_domains,
    } = installation;
    let child_shutdown_remotes = match child_domains
        .iter()
        .map(|child| child.remote.try_clone_task_context())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(remotes) => remotes,
        Err(error) => quarantine_failed_control_session(
            session,
            startup,
            "retain child shutdown wake handles",
            (
                published,
                sources,
                combined_requests,
                child_domains,
                error,
                topology,
                shutdown,
            ),
        ),
    };
    let child_reinit_cells = child_domains
        .iter()
        .map(|child| Arc::clone(&child.reinit))
        .collect::<Vec<_>>();
    let control_io = match (shared_domain, combined_requests) {
        (Some(shared), None) => ControlIoRuntime::Split(shared),
        (None, Some(requests)) => ControlIoRuntime::Combined(requests),
        (None, None) => ControlIoRuntime::None,
        (Some(shared), Some(requests)) => quarantine_failed_control_session(
            session,
            startup,
            "select one shared I/O owner",
            (
                published,
                sources,
                shared,
                requests,
                child_domains,
                topology,
            ),
        ),
    };
    let ready = ControlOwnerReady::from_published(
        &controller_name,
        &published,
        control_remote,
        devices,
        child_domains,
        Arc::clone(&topology),
    );
    if let Err(unaccepted) = startup.publish(Ok(ready)) {
        quarantine_failed_control_session(
            session,
            startup,
            "publish controller installation",
            (unaccepted, published, sources, control_io, topology),
        );
    }
    Ok(service_control_forever(ControlServiceOwner {
        published,
        control_io,
        sources,
        session,
        topology,
        shutdown,
        participant: control_participant,
        child_shutdown_remotes,
        child_reinit_cells,
    }))
}

fn enable_prepared_controller(
    prepared: &mut RdifBlockPreparedOwner,
    sources: &[BoundEvidenceSource],
) -> Result<(), PreparedEnableFailure> {
    for source in sources {
        source.enable().map_err(|owner| PreparedEnableFailure {
            phase: "enable control IRQ action",
            owner: PreparedEnableOwner::Maintenance { _error: owner },
        })?;
    }
    prepared
        .prepared_mut()
        .enable_irq()
        .map_err(|owner| PreparedEnableFailure {
            phase: "enable portable controller IRQ sources",
            owner: PreparedEnableOwner::Driver { _error: owner },
        })?;
    prepared
        .enable_binding_irq()
        .map_err(|owner| PreparedEnableFailure {
            phase: "enable parent IRQ binding",
            owner: PreparedEnableOwner::Binding { _error: owner },
        })
}

struct FinalDomainInstallContext<'owner> {
    controller_name: &'owner str,
    control_activation: ControlDomainActivation,
    publication: &'owner mut ax_driver::block::RdifBlockPublicationOwner,
    session: &'owner MaintenanceSession<V13MaintenanceEvent>,
    control_remote: &'owner DeviceMaintenanceHandle<V13MaintenanceEvent>,
    sources: &'owner mut Vec<BoundEvidenceSource>,
    topology: &'owner Arc<FixedOwnershipTopology>,
    shutdown: &'owner Arc<ControllerShutdown>,
}

fn install_final_domains(
    domains: Vec<UnboundIoDomain>,
    context: FinalDomainInstallContext<'_>,
) -> Result<FinalDomainInstallation, FinalDomainInstallFailure> {
    let FinalDomainInstallContext {
        controller_name,
        control_activation,
        publication,
        session,
        control_remote,
        sources,
        topology,
        shutdown,
    } = context;
    let mut shared_domain = None;
    let mut child_domains = Vec::new();
    let mut next_participant = 1;
    if let ControlDomainActivation::SharedWithIo { domain, .. } = control_activation
        && !domains
            .iter()
            .any(|candidate| candidate.domain_id() == domain)
    {
        let owner_binding = domain_owner_binding(session.owner_cpu(), session.owner_thread())
            .map_err(|()| FinalDomainInstallFailure {
                phase: "construct combined control-domain owner identity",
                previous: None,
                _retained: Box::new(FinalDomainRetained::OwnerIdentity),
            })?;
        publication
            .bind_combined_control_domain(owner_binding)
            .map_err(|error| FinalDomainInstallFailure {
                phase: "bind combined control/I/O ownership domain",
                previous: None,
                _retained: Box::new(FinalDomainRetained::CombinedBinding { _error: error }),
            })?;
    }
    for mut domain in domains {
        match domain_placement(control_activation, domain.domain_id()) {
            DomainPlacement::ControlOwner => {
                if shared_domain.is_some() {
                    return Err(FinalDomainInstallFailure {
                        phase: "install duplicate shared control domain",
                        previous: Some(Box::new(FinalDomainInstallation {
                            shared_domain,
                            child_domains,
                        })),
                        _retained: Box::new(FinalDomainRetained::Unbound {
                            _domain: Box::new(domain),
                        }),
                    });
                }
                let platform_sources = match take_domain_platform_sources(publication, &mut domain)
                {
                    Ok(platform_sources) => platform_sources,
                    Err((platform_sources, error)) => {
                        return Err(FinalDomainInstallFailure {
                            phase: "transfer exact shared-domain IRQ source",
                            previous: Some(Box::new(FinalDomainInstallation {
                                shared_domain,
                                child_domains,
                            })),
                            _retained: Box::new(FinalDomainRetained::PlatformSourceTransfer {
                                _domain: Box::new(domain),
                                _platform_sources: Box::new(platform_sources),
                                _error: error,
                            }),
                        });
                    }
                };
                let (installed, proof) = match install_shared_domain(
                    controller_name,
                    domain,
                    platform_sources,
                    session,
                    sources,
                    topology,
                ) {
                    Ok(installed) => installed,
                    Err(mut failure) => {
                        failure.previous = Some(Box::new(FinalDomainInstallation {
                            shared_domain,
                            child_domains,
                        }));
                        return Err(failure);
                    }
                };
                if let Err(failure) = publication.accept_bound_domain(proof) {
                    return Err(FinalDomainInstallFailure {
                        phase: "accept shared domain proof",
                        previous: Some(Box::new(FinalDomainInstallation {
                            shared_domain,
                            child_domains,
                        })),
                        _retained: Box::new(FinalDomainRetained::SharedProof {
                            _domain: Box::new(installed),
                            _proof: Box::new(failure),
                        }),
                    });
                }
                shared_domain = Some(installed);
            }
            DomainPlacement::IndependentOwner => {
                let platform_sources = match take_domain_platform_sources(publication, &mut domain)
                {
                    Ok(platform_sources) => platform_sources,
                    Err((platform_sources, error)) => {
                        return Err(FinalDomainInstallFailure {
                            phase: "transfer exact independent-domain IRQ source",
                            previous: Some(Box::new(FinalDomainInstallation {
                                shared_domain,
                                child_domains,
                            })),
                            _retained: Box::new(FinalDomainRetained::PlatformSourceTransfer {
                                _domain: Box::new(domain),
                                _platform_sources: Box::new(platform_sources),
                                _error: error,
                            }),
                        });
                    }
                };
                let participant = shutdown
                    .participant(next_participant)
                    .expect("independent domains were counted before owner installation");
                next_participant += 1;
                let control_wake = match control_remote.try_clone_task_context() {
                    Ok(control_wake) => control_wake,
                    Err(error) => {
                        return Err(FinalDomainInstallFailure {
                            phase: "clone control wake for independent domain",
                            previous: Some(Box::new(FinalDomainInstallation {
                                shared_domain,
                                child_domains,
                            })),
                            _retained: Box::new(FinalDomainRetained::Maintenance {
                                _domain: Box::new(domain),
                                _platform_sources: Box::new(platform_sources),
                                _error: error,
                            }),
                        });
                    }
                };
                let installed = match spawn_domain_owner(
                    controller_name,
                    domain,
                    platform_sources,
                    Arc::clone(topology),
                    Arc::clone(shutdown),
                    participant,
                    control_wake,
                ) {
                    Ok(installed) => installed,
                    Err(failure) => {
                        return Err(FinalDomainInstallFailure {
                            phase: failure.phase,
                            previous: Some(Box::new(FinalDomainInstallation {
                                shared_domain,
                                child_domains,
                            })),
                            _retained: Box::new(FinalDomainRetained::Spawn {
                                _owner: failure.retained,
                            }),
                        });
                    }
                };
                if let Err(failure) = publication.accept_bound_domain(installed.proof) {
                    return Err(FinalDomainInstallFailure {
                        phase: "accept independent domain proof",
                        previous: Some(Box::new(FinalDomainInstallation {
                            shared_domain,
                            child_domains,
                        })),
                        _retained: Box::new(FinalDomainRetained::ChildProof {
                            _remote: installed.remote,
                            _thread: installed.thread,
                            _requests: installed.requests,
                            _reinit: installed.reinit,
                            _proof: Box::new(failure),
                        }),
                    });
                }
                child_domains.push(InstalledChildDomain {
                    remote: installed.remote,
                    thread: installed.thread,
                    requests: installed.requests,
                    reinit: installed.reinit,
                });
            }
        }
    }
    Ok(FinalDomainInstallation {
        shared_domain,
        child_domains,
    })
}

fn install_shared_domain(
    controller_name: &str,
    mut domain: UnboundIoDomain,
    mut platform_sources: DomainPlatformSources,
    session: &MaintenanceSession<V13MaintenanceEvent>,
    sources: &mut Vec<BoundEvidenceSource>,
    topology: &Arc<FixedOwnershipTopology>,
) -> Result<(InstalledSharedDomain, rdif_block::BoundDomainProof), FinalDomainInstallFailure> {
    let domain_id = domain.domain_id();
    let requests = match DomainRequestRuntime::new(
        domain_id,
        domain.queues(),
        crate::block::BlockRuntimeConfig::default(),
    ) {
        Ok(requests) => Arc::new(requests),
        Err(error) => {
            return Err(FinalDomainInstallFailure {
                phase: "construct shared domain request runtime",
                previous: None,
                _retained: Box::new(FinalDomainRetained::RequestRuntime {
                    _domain: Box::new(domain),
                    _platform_sources: Box::new(platform_sources),
                    _error: error,
                }),
            });
        }
    };
    let owner = topology
        .domain(domain_id)
        .expect("the shared domain belongs to the fixed topology");
    for portable in domain.irq_sources_mut() {
        match portable {
            IoDomainIrqSource::AlreadyBound(source) => {
                if !sources.iter().any(|bound| bound.source() == *source) {
                    return Err(FinalDomainInstallFailure {
                        phase: "match shared already-bound IRQ source",
                        previous: None,
                        _retained: Box::new(FinalDomainRetained::UnboundWithSources {
                            _domain: Box::new(domain),
                            _platform_sources: Box::new(platform_sources),
                        }),
                    });
                }
            }
            IoDomainIrqSource::New(portable) => {
                let platform_source = platform_sources.take(portable.id());
                let source = match BoundEvidenceSource::register_live_disabled(
                    controller_name,
                    session,
                    owner,
                    portable,
                    platform_source,
                ) {
                    Ok(source) => source,
                    Err(failure) => {
                        return Err(FinalDomainInstallFailure {
                            phase: "register shared domain IRQ source",
                            previous: None,
                            _retained: Box::new(FinalDomainRetained::SourceRegistration {
                                _failure: Box::new(failure),
                                _platform_sources: Box::new(platform_sources),
                            }),
                        });
                    }
                };
                if let Err(error) = source.enable() {
                    return Err(FinalDomainInstallFailure {
                        phase: "enable shared domain IRQ action",
                        previous: None,
                        _retained: Box::new(FinalDomainRetained::SourceEnable {
                            _source: Box::new(source),
                            _platform_sources: Box::new(platform_sources),
                            _error: error,
                        }),
                    });
                }
                sources.push(source);
            }
        }
    }
    if !platform_sources.is_empty() {
        return Err(FinalDomainInstallFailure {
            phase: "match exact platform sources to shared-domain endpoints",
            previous: None,
            _retained: Box::new(FinalDomainRetained::UnmatchedPlatformSources {
                _domain: Box::new(domain),
                _platform_sources: Box::new(platform_sources),
            }),
        });
    }
    let owner_binding = match domain_owner_binding(session.owner_cpu(), session.owner_thread()) {
        Ok(binding) => binding,
        Err(()) => {
            return Err(FinalDomainInstallFailure {
                phase: "construct shared domain owner identity",
                previous: None,
                _retained: Box::new(FinalDomainRetained::Unbound {
                    _domain: Box::new(domain),
                }),
            });
        }
    };
    domain
        .finish_binding(owner_binding)
        .map(|(domain, proof)| {
            (
                InstalledSharedDomain {
                    domain,
                    requests: DomainRequestOwner::new(requests),
                },
                proof,
            )
        })
        .map_err(|failure| FinalDomainInstallFailure {
            phase: "seal shared domain binding",
            previous: None,
            _retained: Box::new(FinalDomainRetained::Install {
                _failure: Box::new(failure),
            }),
        })
}

fn take_domain_platform_sources(
    publication: &ax_driver::block::RdifBlockPublicationOwner,
    domain: &mut UnboundIoDomain,
) -> Result<DomainPlatformSources, (DomainPlatformSources, ax_driver::ExactIrqSourceBindingError)> {
    let source_ids = domain
        .irq_sources_mut()
        .iter()
        .filter_map(|source| match source {
            IoDomainIrqSource::New(source) => Some(source.id()),
            IoDomainIrqSource::AlreadyBound(_) => None,
        })
        .collect::<Vec<_>>();
    let mut platform_sources = DomainPlatformSources::new();
    for source in source_ids {
        match publication.take_exact_irq_source(source.get()) {
            Ok(Some(platform_source)) => platform_sources.push(platform_source),
            Ok(None) => {}
            Err(error) => return Err((platform_sources, error)),
        }
    }
    Ok(platform_sources)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DomainPlacement {
    ControlOwner,
    IndependentOwner,
}

fn domain_placement(
    control: ControlDomainActivation,
    domain: OwnershipDomainId,
) -> DomainPlacement {
    match control {
        ControlDomainActivation::SharedWithIo {
            domain: control_domain,
            ..
        } if control_domain == domain => DomainPlacement::ControlOwner,
        ControlDomainActivation::SharedWithIo { .. }
        | ControlDomainActivation::Independent { .. } => DomainPlacement::IndependentOwner,
    }
}
