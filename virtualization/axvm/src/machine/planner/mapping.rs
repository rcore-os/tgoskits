//! Physical-device disposition and passthrough identity-map holes.

use alloc::{
    boxed::Box,
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    vec::Vec,
};

use axvm_types::VmMachineMode;

use super::{
    PlannedHostDevice, PreconfiguredHostClock, PreconfiguredHostDeviceResources,
    PreconfiguredHostReset,
};
use crate::machine::{
    AddressRange, DeviceDisposition, HostDeviceDependencyKind, HostDeviceOwnership,
    HostDeviceSelector, HostPlatformSnapshot, HostProviderReferenceKind, HostProviderResourceState,
    MachinePlanError, MachinePlanResult, MachineProfile, VmMachineRequest,
    is_planned_host_dependency_substitute,
};

#[derive(Debug, thiserror::Error)]
enum ProviderProtectionError {
    #[error("{0}")]
    SharedProvider(Box<SharedProviderConflict>),
    #[error(
        "host device '{device}' cannot use shared provider '{provider}' through '{property}' \
         selector {specifier:?}: no lease-backed provider grant is available"
    )]
    MissingProviderGrant {
        device: String,
        provider: String,
        property: String,
        specifier: Vec<u32>,
    },
    #[error(
        "host device '{device}' mixes protected and independently configurable clock providers; \
         its assigned-clock properties cannot be projected safely"
    )]
    PartialClockConfiguration { device: String },
}

#[derive(Debug, thiserror::Error)]
#[error(
    "host provider '{provider}' requires mediated access: passthrough device '{assigned_device}' \
     uses '{assigned_property}' selector {assigned_specifier:?}, while protected device \
     '{protected_device}' uses '{protected_property}' selector {protected_specifier:?}"
)]
struct SharedProviderConflict {
    provider: String,
    assigned_device: String,
    assigned_property: String,
    assigned_specifier: Vec<u32>,
    protected_device: String,
    protected_property: String,
    protected_specifier: Vec<u32>,
}

pub(super) fn plan_host_devices(
    mode: VmMachineMode,
    snapshot: &HostPlatformSnapshot,
    denied: &BTreeSet<usize>,
    virtual_templates: &BTreeSet<usize>,
    provider_mediation_supported: bool,
) -> MachinePlanResult<(
    Vec<PlannedHostDevice>,
    Vec<PreconfiguredHostDeviceResources>,
)> {
    if mode == VmMachineMode::Virtual {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut planned = snapshot
        .devices()
        .iter()
        .enumerate()
        .map(|(index, device)| {
            PlannedHostDevice::new(
                device.clone(),
                match device.ownership() {
                    HostDeviceOwnership::HostExclusive => DeviceDisposition::HostExclusive,
                    HostDeviceOwnership::Unrepresentable => DeviceDisposition::Unrepresentable,
                    _ if denied.contains(&index) => DeviceDisposition::Denied,
                    _ if virtual_templates.contains(&index) => {
                        DeviceDisposition::VirtualReplacement
                    }
                    HostDeviceOwnership::Inactive => DeviceDisposition::Inactive,
                    HostDeviceOwnership::Structural
                        if !device.has_physical_resources() || device.assignment().is_some() =>
                    {
                        DeviceDisposition::Structural
                    }
                    HostDeviceOwnership::Structural => DeviceDisposition::Unrepresentable,
                    HostDeviceOwnership::Assignable | HostDeviceOwnership::Transferable => {
                        if device.assignment().is_some() {
                            DeviceDisposition::Passthrough
                        } else {
                            DeviceDisposition::Unrepresentable
                        }
                    }
                },
            )
        })
        .collect::<Vec<_>>();
    let indices = planned
        .iter()
        .enumerate()
        .map(|(index, device)| (device.id().clone(), index))
        .collect::<BTreeMap<_, _>>();
    for device in &planned {
        for dependency in device.dependencies() {
            if !indices.contains_key(dependency.provider()) {
                return Err(MachinePlanError::InvalidFirmware {
                    detail: alloc::format!(
                        "host device '{}' depends on missing provider '{}' through property '{}'",
                        device.id(),
                        dependency.provider(),
                        dependency.property(),
                    ),
                });
            }
        }
    }
    let mut preconfigured =
        protect_host_managed_providers(&mut planned, provider_mediation_supported)?;

    loop {
        let unavailable = planned
            .iter()
            .enumerate()
            .filter(|(_, device)| {
                matches!(
                    device.disposition(),
                    DeviceDisposition::Passthrough | DeviceDisposition::Structural
                )
            })
            .filter_map(|(index, device)| {
                device
                    .dependencies()
                    .iter()
                    .filter(|dependency| dependency.kind() == HostDeviceDependencyKind::Required)
                    .any(|dependency| {
                        let provider = &planned[indices[dependency.provider()]];
                        !matches!(
                            provider.disposition(),
                            DeviceDisposition::Passthrough | DeviceDisposition::Structural
                        ) && !is_preconfigured_dependency(device, dependency, &preconfigured)
                            && !is_planned_host_dependency_substitute(provider.compatibles())
                    })
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        if unavailable.is_empty() {
            break;
        }
        for index in unavailable {
            planned[index].set_disposition(DeviceDisposition::Unrepresentable);
        }
    }
    preconfigured.retain(|resources| {
        indices
            .get(resources.device())
            .is_some_and(|index| planned[*index].disposition() == DeviceDisposition::Passthrough)
    });
    Ok((planned, preconfigured))
}

fn protect_host_managed_providers(
    planned: &mut [PlannedHostDevice],
    provider_mediation_supported: bool,
) -> MachinePlanResult<Vec<PreconfiguredHostDeviceResources>> {
    let protected_providers = protected_provider_indices(planned);
    let protected_provider_ids = protected_providers
        .iter()
        .map(|index| planned[*index].id().clone())
        .collect::<BTreeSet<_>>();
    for provider_index in &protected_providers {
        if provider_has_raw_guest_access(&planned[*provider_index]) {
            planned[*provider_index].set_disposition(DeviceDisposition::Unrepresentable);
        }
    }

    let mut affected_consumers = BTreeSet::new();
    let mut blocked_consumers = BTreeMap::new();
    for provider_index in protected_providers {
        let provider = &planned[provider_index];
        let protected_use = planned.iter().find_map(|consumer| {
            is_host_protected(consumer.disposition())
                .then(|| managed_reference_to(consumer, provider.id()))
                .flatten()
                .map(|dependency| (consumer, dependency))
        });
        for (consumer_index, consumer) in planned
            .iter()
            .enumerate()
            .filter(|(_, consumer)| consumer.disposition() == DeviceDisposition::Passthrough)
        {
            for dependency in consumer.dependencies().iter().filter(|dependency| {
                dependency.provider() == provider.id()
                    && dependency.kind() == HostDeviceDependencyKind::Required
                    && dependency.reference().is_managed()
            }) {
                affected_consumers.insert(consumer_index);
                let conflicting_use = protected_resource_use(planned, dependency);
                if dependency.reference().kind() == HostProviderReferenceKind::ManagedSubresource
                    || conflicting_use.is_some()
                {
                    blocked_consumers.entry(consumer_index).or_insert_with(|| {
                        shared_provider_error(
                            provider,
                            consumer,
                            dependency,
                            conflicting_use.or(protected_use),
                        )
                    });
                }
            }
        }
    }

    let mut preconfigured = Vec::new();
    for consumer_index in affected_consumers {
        let result = blocked_consumers.remove(&consumer_index).map_or_else(
            || {
                preconfigure_host_device(
                    &planned[consumer_index],
                    planned,
                    &protected_provider_ids,
                    provider_mediation_supported,
                )
            },
            Err,
        );
        match result {
            Ok(resources) => preconfigured.push(resources),
            Err(error) => {
                log::warn!(
                    "excluding unsafe passthrough device '{}': {error}",
                    planned[consumer_index].id(),
                );
                planned[consumer_index].set_disposition(DeviceDisposition::Unrepresentable);
            }
        }
    }
    Ok(preconfigured)
}

fn protected_provider_indices(planned: &[PlannedHostDevice]) -> BTreeSet<usize> {
    let mut protected = planned
        .iter()
        .enumerate()
        .filter(|(_, provider)| provider_has_register_aperture(provider))
        .filter(|(_, provider)| {
            is_host_protected(provider.disposition())
                || planned.iter().any(|consumer| {
                    is_host_protected(consumer.disposition())
                        && managed_reference_to(consumer, provider.id()).is_some()
                })
        })
        .map(|(index, _)| index)
        .collect::<BTreeSet<_>>();

    // Firmware node identity is not a physical ownership boundary. Provider
    // helper nodes may describe a subrange of the same register bank, so
    // exposing one of those aliases would bypass the hole around its owner.
    loop {
        let aliases = planned
            .iter()
            .enumerate()
            .filter(|(index, candidate)| {
                !protected.contains(index)
                    && provider_has_register_aperture(candidate)
                    && protected.iter().any(|protected_index| {
                        devices_share_register_aperture(candidate, &planned[*protected_index])
                    })
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if aliases.is_empty() {
            return protected;
        }
        protected.extend(aliases);
    }
}

fn devices_share_register_aperture(left: &PlannedHostDevice, right: &PlannedHostDevice) -> bool {
    left.mmio()
        .iter()
        .any(|left| right.mmio().iter().any(|right| left.overlaps(*right)))
        || left.pio().iter().any(|left| {
            right.pio().iter().any(|right| {
                u32::from(left.base()) < right.end() && u32::from(right.base()) < left.end()
            })
        })
}

fn provider_has_raw_guest_access(provider: &PlannedHostDevice) -> bool {
    matches!(
        provider.disposition(),
        DeviceDisposition::Passthrough | DeviceDisposition::Structural
    ) && provider_has_register_aperture(provider)
}

fn provider_has_register_aperture(provider: &PlannedHostDevice) -> bool {
    !provider.mmio().is_empty() || !provider.pio().is_empty()
}

fn is_host_protected(disposition: DeviceDisposition) -> bool {
    matches!(
        disposition,
        DeviceDisposition::HostExclusive
            | DeviceDisposition::Denied
            | DeviceDisposition::VirtualReplacement
            | DeviceDisposition::Unrepresentable
    )
}

fn managed_reference_to<'a>(
    consumer: &'a PlannedHostDevice,
    provider: &crate::machine::HostDeviceId,
) -> Option<&'a crate::machine::HostDeviceDependency> {
    consumer
        .dependencies()
        .iter()
        .find(|dependency| dependency.provider() == provider && dependency.reference().is_managed())
}

fn protected_resource_use<'a>(
    planned: &'a [PlannedHostDevice],
    assigned: &crate::machine::HostDeviceDependency,
) -> Option<(
    &'a PlannedHostDevice,
    &'a crate::machine::HostDeviceDependency,
)> {
    planned
        .iter()
        .filter(|consumer| is_host_protected(consumer.disposition()))
        .find_map(|consumer| {
            consumer
                .dependencies()
                .iter()
                .find(|dependency| provider_resources_overlap(assigned, dependency))
                .map(|dependency| (consumer, dependency))
        })
}

fn provider_resources_overlap(
    left: &crate::machine::HostDeviceDependency,
    right: &crate::machine::HostDeviceDependency,
) -> bool {
    left.provider() == right.provider()
        && normalized_provider_kind(left.reference().kind())
            == normalized_provider_kind(right.reference().kind())
        && left.reference().specifier() == right.reference().specifier()
}

const fn normalized_provider_kind(kind: HostProviderReferenceKind) -> HostProviderReferenceKind {
    match kind {
        HostProviderReferenceKind::ClockConfiguration => HostProviderReferenceKind::Clock,
        other => other,
    }
}

fn shared_provider_error(
    provider: &PlannedHostDevice,
    assigned_consumer: &PlannedHostDevice,
    assigned_dependency: &crate::machine::HostDeviceDependency,
    protected_use: Option<(&PlannedHostDevice, &crate::machine::HostDeviceDependency)>,
) -> ProviderProtectionError {
    let (protected_device, protected_property, protected_specifier) = protected_use.map_or_else(
        || {
            (
                provider.id().to_string(),
                String::from("provider ownership"),
                Vec::new(),
            )
        },
        |(consumer, dependency)| {
            (
                consumer.id().to_string(),
                dependency.property().into(),
                dependency.reference().specifier().to_vec(),
            )
        },
    );
    ProviderProtectionError::SharedProvider(Box::new(SharedProviderConflict {
        provider: provider.id().to_string(),
        assigned_device: assigned_consumer.id().to_string(),
        assigned_property: assigned_dependency.property().into(),
        assigned_specifier: assigned_dependency.reference().specifier().to_vec(),
        protected_device,
        protected_property,
        protected_specifier,
    }))
}

fn preconfigure_host_device(
    device: &PlannedHostDevice,
    planned: &[PlannedHostDevice],
    protected_providers: &BTreeSet<crate::machine::HostDeviceId>,
    provider_mediation_supported: bool,
) -> Result<PreconfiguredHostDeviceResources, ProviderProtectionError> {
    let clocks = device
        .dependencies()
        .iter()
        .filter(|dependency| {
            dependency.reference().kind() == HostProviderReferenceKind::Clock
                && protected_providers.contains(dependency.provider())
        })
        .map(|dependency| {
            let state = provider_resource_state(planned, device, dependency)?;
            match state {
                HostProviderResourceState::FixedClock(rate_hz) => Ok(PreconfiguredHostClock::new(
                    dependency.provider().clone(),
                    dependency.reference().specifier().to_vec(),
                    rate_hz,
                )),
                HostProviderResourceState::MediatedClock if provider_mediation_supported => {
                    Ok(PreconfiguredHostClock::mediated(
                        dependency.provider().clone(),
                        dependency.reference().specifier().to_vec(),
                    ))
                }
                _ => Err(missing_provider_resource(device, dependency)),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let resets = device
        .dependencies()
        .iter()
        .filter(|dependency| {
            dependency.reference().kind() == HostProviderReferenceKind::Reset
                && protected_providers.contains(dependency.provider())
        })
        .map(|dependency| {
            let state = provider_resource_state(planned, device, dependency)?;
            match state {
                HostProviderResourceState::DeassertedReset => Ok(PreconfiguredHostReset::new(
                    dependency.provider().clone(),
                    dependency.reference().specifier().to_vec(),
                )),
                HostProviderResourceState::MediatedReset if provider_mediation_supported => {
                    Ok(PreconfiguredHostReset::mediated(
                        dependency.provider().clone(),
                        dependency.reference().specifier().to_vec(),
                    ))
                }
                _ => Err(missing_provider_resource(device, dependency)),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let all_clock_configurations = device
        .dependencies()
        .iter()
        .filter(|dependency| {
            dependency.reference().kind() == HostProviderReferenceKind::ClockConfiguration
        })
        .collect::<Vec<_>>();
    let protected_clock_configurations = all_clock_configurations
        .iter()
        .copied()
        .filter(|dependency| protected_providers.contains(dependency.provider()))
        .collect::<Vec<_>>();
    if !protected_clock_configurations.is_empty()
        && protected_clock_configurations.len() != all_clock_configurations.len()
    {
        return Err(ProviderProtectionError::PartialClockConfiguration {
            device: device.id().to_string(),
        });
    }
    let clock_configurations = protected_clock_configurations
        .into_iter()
        .map(|dependency| {
            let state = provider_resource_state(planned, device, dependency)?;
            match state {
                HostProviderResourceState::FixedClock(rate_hz) => Ok(PreconfiguredHostClock::new(
                    dependency.provider().clone(),
                    dependency.reference().specifier().to_vec(),
                    rate_hz,
                )),
                HostProviderResourceState::MediatedClock if provider_mediation_supported => {
                    Ok(PreconfiguredHostClock::mediated(
                        dependency.provider().clone(),
                        dependency.reference().specifier().to_vec(),
                    ))
                }
                _ => Err(missing_provider_resource(device, dependency)),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(first) = clock_configurations.first()
        && clock_configurations
            .iter()
            .any(|clock| clock.is_mediated() != first.is_mediated())
    {
        return Err(ProviderProtectionError::PartialClockConfiguration {
            device: device.id().to_string(),
        });
    }
    Ok(PreconfiguredHostDeviceResources::new(
        device.id().clone(),
        clocks,
        clock_configurations,
        resets,
    ))
}

fn provider_resource_state(
    planned: &[PlannedHostDevice],
    device: &PlannedHostDevice,
    dependency: &crate::machine::HostDeviceDependency,
) -> Result<HostProviderResourceState, ProviderProtectionError> {
    let provider = planned
        .iter()
        .find(|candidate| candidate.id() == dependency.provider())
        .ok_or_else(|| missing_provider_resource(device, dependency))?;
    provider
        .descriptor()
        .provider_resources()
        .iter()
        .find(|grant| {
            let expected_kind = match dependency.reference().kind() {
                HostProviderReferenceKind::ClockConfiguration => HostProviderReferenceKind::Clock,
                kind => kind,
            };
            grant.reference().kind() == expected_kind
                && grant.reference().specifier() == dependency.reference().specifier()
        })
        .map(crate::machine::HostProviderResourceGrant::state)
        .ok_or_else(|| missing_provider_resource(device, dependency))
}

fn missing_provider_resource(
    device: &PlannedHostDevice,
    dependency: &crate::machine::HostDeviceDependency,
) -> ProviderProtectionError {
    ProviderProtectionError::MissingProviderGrant {
        device: device.id().to_string(),
        provider: dependency.provider().to_string(),
        property: dependency.property().into(),
        specifier: dependency.reference().specifier().to_vec(),
    }
}

fn is_preconfigured_dependency(
    device: &PlannedHostDevice,
    dependency: &crate::machine::HostDeviceDependency,
    preconfigured: &[PreconfiguredHostDeviceResources],
) -> bool {
    let Some(resources) = preconfigured
        .iter()
        .find(|resources| resources.device() == device.id())
    else {
        return false;
    };
    match dependency.reference().kind() {
        HostProviderReferenceKind::Clock => resources.clocks().iter().any(|clock| {
            clock.provider() == dependency.provider()
                && clock.specifier() == dependency.reference().specifier()
        }),
        HostProviderReferenceKind::ClockConfiguration => {
            resources.clock_configurations().iter().any(|clock| {
                clock.provider() == dependency.provider()
                    && clock.specifier() == dependency.reference().specifier()
            })
        }
        HostProviderReferenceKind::Reset => resources.resets().iter().any(|reset| {
            reset.provider() == dependency.provider()
                && reset.specifier() == dependency.reference().specifier()
        }),
        HostProviderReferenceKind::Hierarchy
        | HostProviderReferenceKind::InterruptRoute
        | HostProviderReferenceKind::ManagedSubresource => false,
    }
}

pub(super) fn plan_identity_mappings(
    profile: &MachineProfile,
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
    host_devices: &[PlannedHostDevice],
    virtual_holes: &[AddressRange],
) -> Vec<AddressRange> {
    if request.mode() == VmMachineMode::Virtual {
        return Vec::new();
    }

    let mut holes = request.fixed_memory().collect::<Vec<_>>();
    holes.extend_from_slice(profile.reserved_mmio());
    for device in host_devices {
        if matches!(
            device.disposition(),
            DeviceDisposition::HostExclusive
                | DeviceDisposition::Denied
                | DeviceDisposition::VirtualReplacement
                | DeviceDisposition::Unrepresentable
        ) {
            holes.extend_from_slice(device.mmio());
        }
    }
    for selector in request.denied() {
        if let HostDeviceSelector::Mmio(range) = selector {
            holes.push(*range);
        }
    }
    holes.extend_from_slice(virtual_holes);
    let holes = merge_ranges(holes);

    snapshot
        .io_apertures()
        .iter()
        .flat_map(|aperture| subtract_holes(*aperture, &holes))
        .collect()
}

pub(super) fn merge_ranges(mut ranges: Vec<AddressRange>) -> Vec<AddressRange> {
    ranges.sort_by_key(|range| (range.base(), range.end()));
    let mut merged: Vec<AddressRange> = Vec::with_capacity(ranges.len());
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.base() <= last.end()
        {
            let end = last.end().max(range.end());
            if let Some(combined) = AddressRange::from_bounds(last.base(), end) {
                *last = combined;
            }
            continue;
        }
        merged.push(range);
    }
    merged
}

fn subtract_holes(aperture: AddressRange, holes: &[AddressRange]) -> Vec<AddressRange> {
    let mut cursor = aperture.base();
    let mut mappings = Vec::new();
    for hole in holes {
        let Some(hole) = aperture.intersection(*hole) else {
            continue;
        };
        if cursor < hole.base()
            && let Some(mapping) = AddressRange::from_bounds(cursor, hole.base())
        {
            mappings.push(mapping);
        }
        cursor = cursor.max(hole.end());
        if cursor >= aperture.end() {
            break;
        }
    }
    if let Some(mapping) = AddressRange::from_bounds(cursor, aperture.end()) {
        mappings.push(mapping);
    }
    mappings
}
