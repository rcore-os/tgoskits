use super::*;

pub(super) fn select_activation_topology(
    topology: &NvmeActivationTopology,
    plan: &ActivationPlan,
) -> Result<SelectedNvmeTopology, ActivationError> {
    let Some(control) = topology.domains.first() else {
        return Err(ActivationError::MissingOwnershipDomains);
    };
    if plan.control_domain() != control.id {
        return Err(ActivationError::ControlActivationMismatch);
    }

    let mut selected_domains = Vec::with_capacity(topology.domains.len());
    let mut selected_depth = None;
    let mut selected_queue_bits = 0_u64;
    for domain in &topology.domains {
        let Some(selected) = plan.domain(domain.id) else {
            continue;
        };
        let expected_source_bits = 1_u64 << domain.source.get();
        if selected.irq_sources().bits() != expected_source_bits {
            return Err(ActivationError::InvalidIrqSelection { domain: domain.id });
        }
        let depth = usize::from(selected.queue_depth().get());
        if selected_depth
            .replace(depth)
            .is_some_and(|active| active != depth)
        {
            return Err(ActivationError::DriverPreparationFailed {
                code: DriverPrepareErrorCode::UnsupportedTopology,
            });
        }
        let selected_count = usize::from(selected.queue_count().get());
        let mut selected_domain = domain.clone();
        match topology.mode {
            NvmeInterruptMode::SharedIntx => {
                selected_domain.global_queue_slots.truncate(selected_count);
            }
            NvmeInterruptMode::Msix if selected_count != domain.global_queue_slots.len() => {
                return Err(ActivationError::QueueCountOutOfRange { domain: domain.id });
            }
            NvmeInterruptMode::Msix => {}
        }
        if selected_domain.global_queue_slots.len() != selected_count {
            return Err(ActivationError::QueueCountOutOfRange { domain: domain.id });
        }
        for slot in &selected_domain.global_queue_slots {
            let bit = 1_u64.checked_shl(*slot as u32).ok_or(
                ActivationError::DriverPreparationFailed {
                    code: DriverPrepareErrorCode::UnsupportedTopology,
                },
            )?;
            if selected_queue_bits & bit != 0 {
                return Err(ActivationError::DriverPreparationFailed {
                    code: DriverPrepareErrorCode::InvalidState,
                });
            }
            selected_queue_bits |= bit;
        }
        selected_domains.push(selected_domain);
    }
    let queue_count = selected_queue_bits.count_ones() as usize;
    let expected_bits = if queue_count == u64::BITS as usize {
        u64::MAX
    } else {
        (1_u64 << queue_count) - 1
    };
    if selected_queue_bits != expected_bits {
        return Err(ActivationError::DriverPreparationFailed {
            code: DriverPrepareErrorCode::UnsupportedTopology,
        });
    }
    Ok(SelectedNvmeTopology {
        queue_depth: selected_depth.ok_or(ActivationError::MissingOwnershipDomains)?,
        queue_count,
        domains: selected_domains,
    })
}

pub(super) fn build_interrupt_capabilities(
    identity: NonZeroUsize,
    dma_mask: u64,
    queue_depth: HardwareQueueDepth,
    max_queues: NonZeroU16,
    mode: NvmeInterruptMode,
    interrupt_vectors: &[u16],
) -> Result<(ControllerCapabilities, NvmeActivationTopology), ActivationError> {
    let topology = build_activation_topology(mode, max_queues, interrupt_vectors)?;
    let mut domain_capabilities = Vec::with_capacity(topology.domains.len());
    let mut allowed_domain_bits = 0_u64;
    for domain in &topology.domains {
        let mut sources = IdList::none();
        sources.insert(domain.source.get());
        let (min_queues, max_queues, depth) = match topology.mode {
            NvmeInterruptMode::SharedIntx => (NonZeroU16::MIN, max_queues, queue_depth),
            NvmeInterruptMode::Msix => {
                let count = u16::try_from(domain.global_queue_slots.len())
                    .ok()
                    .and_then(NonZeroU16::new)
                    .ok_or(ActivationError::InvalidQueueRange { domain: domain.id })?;
                (count, count, HardwareQueueDepth::fixed(queue_depth.max()))
            }
        };
        let capability = if matches!(topology.mode, NvmeInterruptMode::Msix)
            && domain.id != topology.domains[0].id
        {
            OwnershipDomainCapability::new_optional(
                domain.id,
                LogicalDeviceSelector::AllPublished,
                QueueExecution::Tagged,
                min_queues,
                max_queues,
                depth,
                sources,
            )?
        } else {
            OwnershipDomainCapability::new(
                domain.id,
                LogicalDeviceSelector::AllPublished,
                QueueExecution::Tagged,
                min_queues,
                max_queues,
                depth,
                sources,
            )?
        };
        domain_capabilities.push(capability);
        allowed_domain_bits |= 1_u64 << domain.id.get();
    }
    let control_domain = topology
        .domains
        .first()
        .expect("a validated NVMe topology has a control domain");
    let mut control_sources = IdList::none();
    control_sources.insert(control_domain.source.get());
    let control = ControlDomainCapability::shared_with_io(control_domain.id, control_sources)?;
    let constraints =
        LogicalDeviceConstraints::discover_during_init(DmaDomainId::legacy_global(), dma_mask);
    let capabilities = ControllerCapabilities::new_discovering(
        identity,
        control,
        NonZeroU16::MIN,
        constraints,
        OwnershipDomainIds::from_bits(allowed_domain_bits),
        domain_capabilities,
    )?;
    Ok((capabilities, topology))
}

pub(super) fn build_activation_topology(
    mode: NvmeInterruptMode,
    max_queues: NonZeroU16,
    interrupt_vectors: &[u16],
) -> Result<NvmeActivationTopology, ActivationError> {
    match mode {
        NvmeInterruptMode::SharedIntx => {
            let id = OwnershipDomainId::new(0)?;
            let source = IrqSourceId::new(SHARED_INTX_SOURCE)
                .map_err(|_| ActivationError::InvalidIrqSelection { domain: id })?;
            Ok(NvmeActivationTopology {
                mode,
                domains: vec![NvmeDomainSpec {
                    id,
                    source,
                    global_queue_slots: (0..usize::from(max_queues.get())).collect(),
                }],
            })
        }
        NvmeInterruptMode::Msix => {
            let queue_count = usize::from(max_queues.get());
            if interrupt_vectors.len() < queue_count || interrupt_vectors.first() != Some(&0) {
                return Err(ActivationError::DriverPreparationFailed {
                    code: DriverPrepareErrorCode::UnsupportedTopology,
                });
            }
            let mut domains = Vec::<NvmeDomainSpec>::new();
            for (queue_slot, vector) in interrupt_vectors
                .iter()
                .copied()
                .take(queue_count)
                .enumerate()
            {
                let source_index = usize::from(vector);
                let source = IrqSourceId::new(source_index).map_err(|_| {
                    ActivationError::DriverPreparationFailed {
                        code: DriverPrepareErrorCode::UnsupportedTopology,
                    }
                })?;
                if let Some(domain) = domains.iter_mut().find(|domain| domain.source == source) {
                    domain.global_queue_slots.push(queue_slot);
                    continue;
                }
                let id = OwnershipDomainId::new(domains.len())?;
                domains.push(NvmeDomainSpec {
                    id,
                    source,
                    global_queue_slots: vec![queue_slot],
                });
            }
            Ok(NvmeActivationTopology { mode, domains })
        }
    }
}

pub(super) const fn prepare_error_code(error: &Error) -> DriverPrepareErrorCode {
    match error {
        Error::NoMemory | Error::Layout => DriverPrepareErrorCode::NoMemory,
        Error::Dma(_) | Error::Mmio(_) => DriverPrepareErrorCode::ResourceUnavailable,
        Error::Activation(_) | Error::Unknown(_) => DriverPrepareErrorCode::InvalidState,
    }
}

pub(super) const fn block_prepare_error_code(error: BlkError) -> DriverPrepareErrorCode {
    match error {
        BlkError::NoMemory => DriverPrepareErrorCode::NoMemory,
        BlkError::NotSupported => DriverPrepareErrorCode::UnsupportedTopology,
        BlkError::Offline | BlkError::Quarantined => DriverPrepareErrorCode::ResourceUnavailable,
        _ => DriverPrepareErrorCode::InvalidState,
    }
}

pub(super) const fn domain_slot_for_hardware_qid(qid: u32) -> Option<usize> {
    let Some(slot) = qid.checked_sub(1) else {
        return None;
    };
    if slot < u64::BITS {
        Some(slot as usize)
    } else {
        None
    }
}

pub(super) fn build_namespace_publication(
    name: &'static str,
    namespace: Option<Namespace>,
    dma_mask: u64,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
) -> Result<(Vec<DriverLogicalDeviceDesc>, Vec<NvmeNamespaceRoute>), InitError> {
    let Some(namespace) = namespace else {
        return Ok((Vec::new(), Vec::new()));
    };
    let namespace_id = NonZeroU64::new(u64::from(namespace.id))
        .ok_or(InitError::Hardware("NVMe published namespace ID zero"))?;
    let driver_key = DriverDeviceKey::new(namespace_id);
    let queue_limits = hardware_limits(dma_mask, page_size, max_transfer_bytes, namespace);
    Ok((
        vec![DriverLogicalDeviceDesc::new(
            driver_key,
            name,
            device_info(name, namespace),
            queue_limits,
        )],
        vec![NvmeNamespaceRoute::new(
            driver_key,
            namespace,
            max_transfer_bytes,
        )],
    ))
}
