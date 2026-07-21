use super::*;

pub(super) fn validate_capabilities(
    control: ControlDomainCapability,
    publication: &LogicalDevicePublicationContract,
    domains: &[OwnershipDomainCapability],
) -> Result<(), ActivationError> {
    if domains.is_empty() {
        return Err(ActivationError::MissingOwnershipDomains);
    }
    let mut domain_bits = 0_u64;
    let mut source_owners = [None; MAX_CONTROLLER_IRQ_SOURCES];
    for domain in domains {
        let bit = 1_u64 << domain.id.get();
        if domain_bits & bit != 0 {
            return Err(ActivationError::DuplicateOwnershipDomain { domain: domain.id });
        }
        domain_bits |= bit;
        for source_id in domain.irq_sources.iter() {
            if let Some(first_domain) = source_owners[source_id] {
                return Err(ActivationError::OverlappingDomainIrqSource {
                    source_id,
                    first_domain,
                    second_domain: domain.id,
                });
            }
            source_owners[source_id] = Some(domain.id);
        }
    }

    match publication {
        LogicalDevicePublicationContract::Exact(devices) => {
            validate_exact_capabilities(devices, domains)?;
        }
        LogicalDevicePublicationContract::Discover {
            allowed_domains, ..
        } => {
            for domain in domains {
                if !allowed_domains.contains(domain.id)
                    || !matches!(domain.logical_devices, LogicalDeviceSelector::AllPublished)
                {
                    return Err(ActivationError::DiscoverDomainContractMismatch {
                        domain: domain.id,
                    });
                }
            }
            for domain_index in 0..MAX_OWNERSHIP_DOMAINS {
                let domain = OwnershipDomainId::new(domain_index)?;
                if allowed_domains.contains(domain)
                    && domains.iter().all(|candidate| candidate.id != domain)
                {
                    return Err(ActivationError::UnknownDiscoverDomain { domain });
                }
            }
        }
    }
    validate_control_capability(control, domains)
}

fn validate_exact_capabilities(
    devices: &[LogicalDeviceCapability],
    domains: &[OwnershipDomainCapability],
) -> Result<(), ActivationError> {
    if devices.is_empty() {
        return Err(ActivationError::MissingLogicalDevices);
    }
    for (index, device) in devices.iter().enumerate() {
        if devices[..index]
            .iter()
            .any(|candidate| candidate.driver_key == device.driver_key)
        {
            return Err(ActivationError::DuplicateDriverDeviceKey {
                key: device.driver_key,
            });
        }
        if !domains
            .iter()
            .any(|domain| domain.logical_devices.contains(device.driver_key))
        {
            return Err(ActivationError::UnassignedDriverDeviceKey {
                key: device.driver_key,
            });
        }
    }
    for domain in domains {
        if let LogicalDeviceSelector::Exact(keys) = &domain.logical_devices {
            for key in keys {
                if !devices.iter().any(|device| device.driver_key == *key) {
                    return Err(ActivationError::UndeclaredDomainDriverKey {
                        domain: domain.id,
                        key: *key,
                    });
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_activation_plan(
    capabilities: &ControllerCapabilities,
    control: ControlDomainActivation,
    plans: &[DomainActivationPlan],
) -> Result<(), ActivationError> {
    let mut seen = 0_u64;
    for plan in plans {
        let Some(capability) = capabilities.domain(plan.domain) else {
            return Err(ActivationError::UnknownOwnershipDomain {
                domain: plan.domain,
            });
        };
        let bit = 1_u64 << plan.domain.get();
        if seen & bit != 0 {
            return Err(ActivationError::DuplicateDomainPlan {
                domain: plan.domain,
            });
        }
        seen |= bit;
        if plan.queue_count < capability.min_queues || plan.queue_count > capability.max_queues {
            return Err(ActivationError::QueueCountOutOfRange {
                domain: plan.domain,
            });
        }
        if !capability.queue_depth.contains(plan.queue_depth) {
            return Err(ActivationError::QueueDepthOutOfRange {
                domain: plan.domain,
            });
        }
        if plan.irq_sources.is_empty()
            || plan.irq_sources.bits() & !capability.irq_sources.bits() != 0
        {
            return Err(ActivationError::InvalidIrqSelection {
                domain: plan.domain,
            });
        }
    }
    for capability in &capabilities.domains {
        if capability.is_required() && seen & (1_u64 << capability.id.get()) == 0 {
            return Err(ActivationError::MissingDomainPlan {
                domain: capability.id,
            });
        }
    }
    validate_control_activation(capabilities.control, control, plans)
}

fn validate_control_capability(
    control: ControlDomainCapability,
    domains: &[OwnershipDomainCapability],
) -> Result<(), ActivationError> {
    match control {
        ControlDomainCapability::SharedWithIo {
            domain,
            irq_sources,
        } => {
            let Some(io_domain) = domains.iter().find(|candidate| candidate.id == domain) else {
                return Err(ActivationError::SharedControlDomainMissing { domain });
            };
            if !io_domain.is_required() {
                return Err(ActivationError::OptionalSharedControlDomain { domain });
            }
            if irq_sources.is_empty() || irq_sources.bits() & !io_domain.irq_sources.bits() != 0 {
                return Err(ActivationError::ControlIrqCapabilityMismatch { domain });
            }
        }
        ControlDomainCapability::Independent {
            domain,
            irq_sources,
        } => {
            if irq_sources.is_empty() {
                return Err(ActivationError::EmptyControlIrqSet { domain });
            }
            if domains.iter().any(|candidate| candidate.id == domain) {
                return Err(ActivationError::IndependentControlDomainCollides { domain });
            }
            let io_sources = domains
                .iter()
                .fold(IdList::none(), |mut sources, candidate| {
                    for source in candidate.irq_sources.iter() {
                        sources.insert(source);
                    }
                    sources
                });
            if irq_sources.bits() & io_sources.bits() != 0 {
                return Err(ActivationError::IndependentControlIrqOverlaps { domain });
            }
        }
    }
    Ok(())
}

fn validate_control_activation(
    capability: ControlDomainCapability,
    selected: ControlDomainActivation,
    plans: &[DomainActivationPlan],
) -> Result<(), ActivationError> {
    let (cap_domain, cap_sources, selected_domain, selected_sources) = match (capability, selected)
    {
        (
            ControlDomainCapability::SharedWithIo {
                domain: cap_domain,
                irq_sources: cap_sources,
            },
            ControlDomainActivation::SharedWithIo {
                domain: selected_domain,
                irq_sources: selected_sources,
            },
        )
        | (
            ControlDomainCapability::Independent {
                domain: cap_domain,
                irq_sources: cap_sources,
            },
            ControlDomainActivation::Independent {
                domain: selected_domain,
                irq_sources: selected_sources,
            },
        ) => (cap_domain, cap_sources, selected_domain, selected_sources),
        _ => return Err(ActivationError::ControlActivationMismatch),
    };
    if cap_domain != selected_domain
        || selected_sources.is_empty()
        || selected_sources.bits() & !cap_sources.bits() != 0
    {
        return Err(ActivationError::ControlActivationMismatch);
    }
    if matches!(selected, ControlDomainActivation::SharedWithIo { .. })
        && plans
            .iter()
            .find(|plan| plan.domain == selected_domain)
            .is_none_or(|plan| selected_sources.bits() & !plan.irq_sources.bits() != 0)
    {
        return Err(ActivationError::ControlActivationMismatch);
    }
    Ok(())
}

pub(super) fn validate_control_irq_parts(
    domain: OwnershipDomainId,
    irq_sources: &[DomainIrqSource],
) -> Result<(), ActivationError> {
    if irq_sources.is_empty() {
        return Err(ActivationError::EmptyControlIrqSet { domain });
    }
    let mut seen = IdList::none();
    for source in irq_sources {
        let source_id = source.id().get();
        if seen.contains(source_id) {
            return Err(ActivationError::DuplicateControlIrqSource { domain, source_id });
        }
        seen.insert(source_id);
    }
    Ok(())
}

pub(super) fn validate_io_domain_part(
    id: OwnershipDomainId,
    queues: &[InterruptQueueDesc],
    irq_sources: &[IoDomainIrqSource],
    io: &dyn InterruptIoDomain,
) -> Result<(), ActivationError> {
    let mut source_ids = IdList::none();
    for source in irq_sources {
        let source_id = source.id().get();
        if source_ids.contains(source_id) {
            return Err(ActivationError::DuplicateDomainIrqSource {
                domain: id,
                source_id,
            });
        }
        source_ids.insert(source_id);
    }
    validate_io_domain_shape(id, queues, source_ids, io)
}

pub(super) fn validate_combined_io_domain_part(
    id: OwnershipDomainId,
    queues: &[InterruptQueueDesc],
    irq_sources: &[DomainIrqSource],
    io: &dyn InterruptIoDomain,
) -> Result<(), ActivationError> {
    let mut source_ids = IdList::none();
    for source in irq_sources {
        let source_id = source.id().get();
        if source_ids.contains(source_id) {
            return Err(ActivationError::DuplicateDomainIrqSource {
                domain: id,
                source_id,
            });
        }
        source_ids.insert(source_id);
    }
    validate_io_domain_shape(id, queues, source_ids, io)
}

fn validate_io_domain_shape(
    id: OwnershipDomainId,
    queues: &[InterruptQueueDesc],
    source_ids: IdList,
    io: &dyn InterruptIoDomain,
) -> Result<(), ActivationError> {
    if io.domain_id() != id {
        return Err(ActivationError::IoDomainIdentityMismatch);
    }
    if io.queue_count() != queues.len() {
        return Err(ActivationError::IoDomainQueueCountMismatch { domain: id });
    }
    let mut queue_ids = 0_u64;
    for queue in queues {
        if queue.ownership_domain != id {
            return Err(ActivationError::QueueOwnershipMismatch {
                domain: id,
                queue_id: queue.id,
            });
        }
        let bit = 1_u64 << queue.id;
        if queue_ids & bit != 0 {
            return Err(ActivationError::DuplicateDomainQueue {
                domain: id,
                queue_id: queue.id,
            });
        }
        queue_ids |= bit;
        if queue.irq_sources.bits() & !source_ids.bits() != 0 {
            return Err(ActivationError::QueueIrqSourceUnbound {
                domain: id,
                queue_id: queue.id,
            });
        }
    }
    Ok(())
}

pub(super) fn validate_prepared_parts(
    plan: &ActivationPlan,
    control: &ControllerControlPart,
) -> Result<(), ActivationError> {
    if control.controller_identity() != plan.controller_identity {
        return Err(ActivationError::ControllerIdentityMismatch);
    }
    if control.control_domain() != plan.control_activation.domain() {
        return Err(ActivationError::ControlDomainMismatch);
    }
    match (plan.control_activation, &control.irq_ownership) {
        (
            ControlDomainActivation::SharedWithIo { irq_sources, .. },
            ControlIrqOwnership::SharedWithIo(actual_sources),
        ) => {
            if control_source_set(actual_sources) != irq_sources {
                return Err(ActivationError::ControlIrqSourceMismatch);
            }
        }
        (
            ControlDomainActivation::Independent { irq_sources, .. },
            ControlIrqOwnership::Independent(actual_sources),
        ) => {
            let actual_sources = control_source_set(actual_sources);
            if actual_sources != irq_sources {
                return Err(ActivationError::ControlIrqSourceMismatch);
            }
        }
        _ => return Err(ActivationError::ControlIrqOwnershipMismatch),
    }
    Ok(())
}

pub(super) fn validate_ready_parts(
    plan: &ActivationPlan,
    logical_devices: &[DriverLogicalDeviceDesc],
    io_domains: &[IoDomainPart],
    require_bound_irq: bool,
) -> Result<(), ActivationError> {
    validate_driver_publication(&plan.publication, logical_devices)?;
    validate_realized_domains(plan, logical_devices, io_domains, None)?;
    if require_bound_irq
        && io_domains.iter().any(|domain| {
            domain.irq_sources.iter().any(
                |source| matches!(source, IoDomainIrqSource::New(source) if !source.is_bound()),
            )
        })
    {
        return Err(ActivationError::IoIrqSourceNotBound);
    }
    Ok(())
}

pub(super) fn validate_combined_ready_parts(
    plan: &ActivationPlan,
    logical_devices: &[DriverLogicalDeviceDesc],
    independent_domains: &[IoDomainPart],
) -> Result<(), ActivationError> {
    let ControlDomainActivation::SharedWithIo { domain, .. } = plan.control_activation else {
        return Err(ActivationError::ControlIrqOwnershipMismatch);
    };
    if independent_domains
        .iter()
        .any(|candidate| candidate.id == domain)
    {
        return Err(ActivationError::DuplicateActivatedDomain { domain });
    }
    validate_driver_publication(&plan.publication, logical_devices)?;
    for selected in &plan.domains {
        if selected.domain != domain
            && independent_domains
                .iter()
                .all(|candidate| candidate.id != selected.domain)
        {
            return Err(ActivationError::ActivatedDomainMismatch {
                domain: selected.domain,
            });
        }
    }
    Ok(())
}

pub(super) fn validate_combined_ready_parts_with_control(
    plan: &ActivationPlan,
    logical_devices: &[DriverLogicalDeviceDesc],
    independent_domains: &[IoDomainPart],
    control: &ControllerControlPart,
) -> Result<(), ActivationError> {
    let Some(queues) = control.combined_queues() else {
        return Err(ActivationError::ControlIrqOwnershipMismatch);
    };
    let sources = match &control.irq_ownership {
        ControlIrqOwnership::SharedWithIo(sources) => control_source_set(sources),
        ControlIrqOwnership::Independent(_) => {
            return Err(ActivationError::ControlIrqOwnershipMismatch);
        }
    };
    validate_driver_publication(&plan.publication, logical_devices)?;
    validate_realized_domains(
        plan,
        logical_devices,
        independent_domains,
        Some((control.control_domain, queues, sources)),
    )
}

fn validate_driver_publication(
    contract: &LogicalDevicePublicationContract,
    logical_devices: &[DriverLogicalDeviceDesc],
) -> Result<(), ActivationError> {
    for (index, logical_device) in logical_devices.iter().enumerate() {
        if logical_devices[..index]
            .iter()
            .any(|candidate| candidate.driver_key == logical_device.driver_key)
        {
            return Err(ActivationError::DuplicateDriverDeviceKey {
                key: logical_device.driver_key,
            });
        }
        validate_driver_device(logical_device)?;
    }
    match contract {
        LogicalDevicePublicationContract::Exact(capabilities) => {
            if logical_devices.len() != capabilities.len() {
                return Err(ActivationError::PublicationDriverKeySetMismatch);
            }
            for capability in capabilities {
                let Some(device) = logical_devices
                    .iter()
                    .find(|device| device.driver_key == capability.driver_key)
                else {
                    return Err(ActivationError::PublicationDriverKeySetMismatch);
                };
                validate_constraints(device, capability.constraints)?;
            }
        }
        LogicalDevicePublicationContract::Discover {
            max_devices,
            constraints,
            ..
        } => {
            if logical_devices.len() > usize::from(max_devices.get()) {
                return Err(ActivationError::PublishedDeviceLimitExceeded);
            }
            for device in logical_devices {
                validate_constraints(device, *constraints)?;
            }
        }
    }
    Ok(())
}

fn validate_driver_device(device: &DriverLogicalDeviceDesc) -> Result<(), ActivationError> {
    let geometry = device.device;
    let limits = device.limits;
    if geometry.num_blocks == 0
        || geometry.logical_block_size == 0
        || !geometry.logical_block_size.is_power_of_two()
        || limits.dma_alignment == 0
        || !limits.dma_alignment.is_power_of_two()
        || limits.max_blocks_per_request == 0
        || limits.max_segments == 0
        || limits.max_segment_size < geometry.logical_block_size
    {
        return Err(ActivationError::InvalidPublishedDriverDevice {
            key: device.driver_key,
        });
    }
    Ok(())
}

fn validate_constraints(
    device: &DriverLogicalDeviceDesc,
    constraints: LogicalDeviceConstraints,
) -> Result<(), ActivationError> {
    if device.limits.dma_domain != constraints.dma_domain
        || device.limits.dma_mask != constraints.dma_mask
    {
        return Err(ActivationError::PublishedDriverDeviceConstraintViolation {
            key: device.driver_key,
        });
    }
    Ok(())
}

fn validate_realized_domains(
    plan: &ActivationPlan,
    logical_devices: &[DriverLogicalDeviceDesc],
    io_domains: &[IoDomainPart],
    combined: Option<(OwnershipDomainId, &[InterruptQueueDesc], IdList)>,
) -> Result<(), ActivationError> {
    let mut state = RealizedValidationState::new();
    for domain in io_domains {
        let actual_sources = io_source_set(&domain.irq_sources);
        validate_already_bound_sources(plan, domain)?;
        validate_realized_domain(
            plan,
            logical_devices,
            domain.id,
            &domain.queues,
            actual_sources,
            &mut state,
        )?;
    }
    if let Some((domain, queues, sources)) = combined {
        let ControlDomainActivation::SharedWithIo {
            domain: control_domain,
            irq_sources,
        } = plan.control_activation
        else {
            return Err(ActivationError::ControlIrqOwnershipMismatch);
        };
        if domain != control_domain || sources != irq_sources {
            return Err(ActivationError::ControlIrqSourceMismatch);
        }
        validate_realized_domain(plan, logical_devices, domain, queues, sources, &mut state)?;
    }
    for selected in &plan.domains {
        if state.domain_bits & (1_u64 << selected.domain.get()) == 0 {
            return Err(ActivationError::ActivatedDomainMismatch {
                domain: selected.domain,
            });
        }
    }
    for device in logical_devices {
        let routed_by_split = io_domains.iter().any(|domain| {
            domain
                .queues
                .iter()
                .any(|queue| queue.logical_devices.contains(device.driver_key))
        });
        let routed_by_combined = combined.is_some_and(|(_, queues, _)| {
            queues
                .iter()
                .any(|queue| queue.logical_devices.contains(device.driver_key))
        });
        if !routed_by_split && !routed_by_combined {
            return Err(ActivationError::UnroutedDriverDevice {
                key: device.driver_key,
            });
        }
    }
    Ok(())
}

struct RealizedValidationState {
    domain_bits: u64,
    queue_bits: u64,
    source_owners: [Option<OwnershipDomainId>; MAX_CONTROLLER_IRQ_SOURCES],
}

impl RealizedValidationState {
    const fn new() -> Self {
        Self {
            domain_bits: 0,
            queue_bits: 0,
            source_owners: [None; MAX_CONTROLLER_IRQ_SOURCES],
        }
    }
}

fn validate_realized_domain(
    plan: &ActivationPlan,
    logical_devices: &[DriverLogicalDeviceDesc],
    domain: OwnershipDomainId,
    queues: &[InterruptQueueDesc],
    sources: IdList,
    state: &mut RealizedValidationState,
) -> Result<(), ActivationError> {
    let bit = 1_u64 << domain.get();
    if state.domain_bits & bit != 0 {
        return Err(ActivationError::DuplicateActivatedDomain { domain });
    }
    state.domain_bits |= bit;
    let Some(selected) = plan.domain(domain) else {
        return Err(ActivationError::ActivatedDomainMismatch { domain });
    };
    let Some(capability) = plan.domain_capability(domain) else {
        return Err(ActivationError::ActivatedDomainMismatch { domain });
    };
    for source_id in sources.iter() {
        if let Some(first_domain) = state.source_owners[source_id] {
            return Err(ActivationError::OverlappingDomainIrqSource {
                source_id,
                first_domain,
                second_domain: domain,
            });
        }
        state.source_owners[source_id] = Some(domain);
    }
    if usize::from(selected.queue_count.get()) != queues.len() || selected.irq_sources != sources {
        return Err(ActivationError::ActivatedDomainMismatch { domain });
    }
    for queue in queues {
        validate_queue_selector(queue, capability, logical_devices)?;
        if queue.execution != capability.execution {
            return Err(ActivationError::ActivatedQueueExecutionMismatch { queue_id: queue.id });
        }
        if queue.queue_depth != selected.queue_depth {
            return Err(ActivationError::ActivatedQueueDepthMismatch { queue_id: queue.id });
        }
        if queue.irq_sources.bits() & !selected.irq_sources.bits() != 0 {
            return Err(ActivationError::ActivatedQueueIrqSelectionMismatch { queue_id: queue.id });
        }
        let bit = 1_u64 << queue.id;
        if state.queue_bits & bit != 0 {
            return Err(ActivationError::DuplicateActivatedQueue { queue_id: queue.id });
        }
        state.queue_bits |= bit;
    }
    Ok(())
}

fn validate_queue_selector(
    queue: &InterruptQueueDesc,
    capability: &OwnershipDomainCapability,
    logical_devices: &[DriverLogicalDeviceDesc],
) -> Result<(), ActivationError> {
    match &queue.logical_devices {
        LogicalDeviceSelector::Exact(keys) => {
            for key in keys {
                if !capability.logical_devices.contains(*key)
                    || !logical_devices
                        .iter()
                        .any(|device| device.driver_key == *key)
                {
                    return Err(ActivationError::ActivatedQueueDriverKeyMismatch {
                        queue_id: queue.id,
                        key: *key,
                    });
                }
            }
        }
        LogicalDeviceSelector::AllPublished => {
            if !matches!(
                capability.logical_devices,
                LogicalDeviceSelector::AllPublished
            ) {
                return Err(ActivationError::ActivatedQueueSelectorTooBroad { queue_id: queue.id });
            }
        }
        LogicalDeviceSelector::Unrouted => {}
    }
    Ok(())
}

pub(super) fn validate_publication_ready(
    plan: &ActivationPlan,
    ready: &ControllerPublicationReady,
) -> Result<(), ActivationError> {
    if ready.controller_identity != plan.controller_identity {
        return Err(ActivationError::PublicationIdentityMismatch);
    }
    if ready.combined_shared_domain {
        validate_combined_ready_parts(plan, &ready.logical_devices, &ready.io_domains)
    } else {
        validate_ready_parts(plan, &ready.logical_devices, &ready.io_domains, true)
    }
}

pub(super) fn validate_driver_control_poll(
    plan: &ActivationPlan,
    expected_evidence: bool,
    result: &DriverControlPoll,
) -> Result<(), ActivationError> {
    match (expected_evidence, result.evidence()) {
        (true, None) => return Err(ActivationError::MissingControlEvidenceDisposition),
        (false, Some(_)) => return Err(ActivationError::UnexpectedControlEvidenceDisposition),
        _ => {}
    }
    let has_undrained_evidence = result
        .evidence()
        .is_some_and(|evidence| !matches!(evidence, EvidenceServiceResult::Drained));
    if matches!(result.progress(), ControlProgress::PublicationReady(_)) && has_undrained_evidence {
        return Err(ActivationError::PublicationWithUndrainedEvidence);
    }
    if matches!(result.progress(), ControlProgress::Reinitialized(_)) && has_undrained_evidence {
        return Err(ActivationError::ReinitializationWithUndrainedEvidence);
    }
    if let ControlProgress::Pending(schedule) = result.progress() {
        let owned = plan.control_activation.irq_sources();
        if schedule.irq_sources().bits() & !owned.bits() != 0 {
            return Err(ActivationError::ControlScheduleIrqSourceMismatch {
                scheduled: schedule.irq_sources(),
                owned,
            });
        }
    }
    if let ControlProgress::Reinitialized(reinitialized) = result.progress() {
        let mut seen = 0_u64;
        for permit in reinitialized.domains() {
            if plan.domain(permit.domain()).is_none() {
                return Err(ActivationError::UnknownReinitDomain {
                    domain: permit.domain(),
                });
            }
            seen |= 1_u64 << permit.domain().get();
        }
        if plan
            .domains
            .iter()
            .any(|domain| seen & (1_u64 << domain.domain.get()) == 0)
        {
            return Err(ActivationError::MissingReinitDomainPermit);
        }
    }
    Ok(())
}

pub(super) fn materialize_logical_devices(
    mut driver_devices: Vec<DriverLogicalDeviceDesc>,
) -> Vec<LogicalDeviceDesc> {
    driver_devices.sort_unstable_by_key(|device| device.driver_key);
    driver_devices
        .into_iter()
        .enumerate()
        .map(|(runtime_id, device)| LogicalDeviceDesc {
            id: LogicalDeviceId::new(runtime_id)
                .unwrap_or_else(|_| unreachable!("publication validation bounds runtime IDs")),
            driver_key: device.driver_key,
            name: device.name,
            device: device.device,
            limits: device.limits,
        })
        .collect()
}

pub(super) fn build_logical_device_routes(
    logical_devices: &[LogicalDeviceDesc],
    io_domains: &[IoDomainPart],
) -> Vec<LogicalDeviceRoute> {
    build_logical_device_routes_with_combined(logical_devices, io_domains, None)
}

pub(super) fn build_logical_device_routes_with_combined(
    logical_devices: &[LogicalDeviceDesc],
    io_domains: &[IoDomainPart],
    combined: Option<(OwnershipDomainId, &[InterruptQueueDesc])>,
) -> Vec<LogicalDeviceRoute> {
    logical_devices
        .iter()
        .map(|logical_device| {
            let mut ownership_domains = 0_u64;
            let mut queues = IdList::none();
            for domain in io_domains {
                for queue in &domain.queues {
                    if queue.logical_devices.contains(logical_device.driver_key) {
                        ownership_domains |= 1_u64 << domain.id.get();
                        queues.insert(queue.id);
                    }
                }
            }
            if let Some((domain, combined_queues)) = combined {
                for queue in combined_queues {
                    if queue.logical_devices.contains(logical_device.driver_key) {
                        ownership_domains |= 1_u64 << domain.get();
                        queues.insert(queue.id);
                    }
                }
            }
            LogicalDeviceRoute {
                runtime_id: logical_device.id,
                driver_key: logical_device.driver_key,
                ownership_domains,
                queues,
            }
        })
        .collect()
}

fn control_source_set(sources: &[DomainIrqSource]) -> IdList {
    sources.iter().fold(IdList::none(), |mut ids, source| {
        ids.insert(source.id.get());
        ids
    })
}

fn io_source_set(sources: &[IoDomainIrqSource]) -> IdList {
    sources.iter().fold(IdList::none(), |mut ids, source| {
        ids.insert(source.id().get());
        ids
    })
}

fn validate_already_bound_sources(
    plan: &ActivationPlan,
    domain: &IoDomainPart,
) -> Result<(), ActivationError> {
    for source in &domain.irq_sources {
        let IoDomainIrqSource::AlreadyBound(source) = source else {
            continue;
        };
        let ControlDomainActivation::SharedWithIo {
            domain: control_domain,
            irq_sources,
        } = plan.control_activation
        else {
            return Err(ActivationError::UnexpectedAlreadyBoundIoSource {
                domain: domain.id,
                source_id: source.get(),
            });
        };
        if domain.id != control_domain || !irq_sources.contains(source.get()) {
            return Err(ActivationError::UnexpectedAlreadyBoundIoSource {
                domain: domain.id,
                source_id: source.get(),
            });
        }
    }
    Ok(())
}
