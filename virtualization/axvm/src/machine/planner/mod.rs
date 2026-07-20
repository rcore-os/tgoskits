//! Deterministic VM machine planning.

mod allocation;
mod mapping;
mod output;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::ToString,
    vec::Vec,
};

use axdevice::{DeviceBackend, MsiDeviceId};
use axvm_types::VmMachineMode;
pub use output::*;

use self::{
    allocation::{ResourceAllocators, resolve_virtual_device},
    mapping::{plan_host_devices, plan_identity_mappings},
};
use super::{
    DeviceDisposition, HostDeviceId, HostDeviceSelector, HostInterruptResource,
    HostPlatformSnapshot, MachinePlanError, MachinePlanResult, MachineProfile,
    VirtualDeviceDescriptor, VirtualDeviceSource, VmMachineRequest, resolve_interrupt_controller,
};

/// Builds one deterministic machine plan from immutable inputs.
#[derive(Clone, Debug)]
pub struct VmMachinePlanner {
    profile: MachineProfile,
}

impl VmMachinePlanner {
    /// Creates a planner for one architecture profile.
    pub const fn new(profile: MachineProfile) -> Self {
        Self { profile }
    }

    /// Validates and resolves all devices, resources, ownership, and mappings.
    pub fn plan(
        &self,
        request: &VmMachineRequest,
        snapshot: &HostPlatformSnapshot,
    ) -> MachinePlanResult<VmMachinePlan> {
        validate_request(request)?;

        let denied_devices = resolve_denied_devices(request, snapshot)?;
        let mut allocators = ResourceAllocators::new(&self.profile, request, snapshot)?;
        let mut consumed_templates = BTreeSet::new();
        let mut virtual_devices = request.virtual_devices().to_vec();
        virtual_devices.sort_by(|left, right| left.instance_id().cmp(right.instance_id()));

        let mut resolved_devices = Vec::with_capacity(virtual_devices.len());
        let mut virtual_holes = Vec::new();
        for (device_index, device) in virtual_devices.iter().enumerate() {
            let template_index =
                select_template(request.mode(), device, snapshot, &consumed_templates)?;
            if let Some(index) = template_index {
                consumed_templates.insert(index);
            }
            let resolved = resolve_virtual_device(
                device,
                template_index.map(|index| &snapshot.devices()[index]),
                &mut allocators,
                MsiDeviceId::new(device_index as u32),
            )?;
            if let Some(index) = template_index {
                virtual_holes.extend_from_slice(snapshot.devices()[index].mmio());
            }
            virtual_holes.extend(resolved.mmio().iter().map(ResolvedMmio::range));
            resolved_devices.push(resolved);
        }

        let interrupt_controller =
            resolve_interrupt_controller(self.profile.interrupt_controller(), request, snapshot)?;
        let host_devices = plan_host_devices(
            request.mode(),
            snapshot,
            &denied_devices,
            &consumed_templates,
        )?;
        let assigned_host_interrupts = resolve_assigned_host_interrupts(&host_devices)?;
        let claims = host_devices
            .iter()
            .filter(|device| device.requires_claim())
            .map(|device| device.id().clone())
            .collect();
        let identity_mappings = plan_identity_mappings(
            &self.profile,
            request,
            snapshot,
            &host_devices,
            &virtual_holes,
        );
        Ok(VmMachinePlan::from_parts(VmMachinePlanParts {
            snapshot_generation: snapshot.generation(),
            host_console: snapshot.console_device().cloned(),
            mode: request.mode(),
            firmware: request.firmware(),
            physical_interrupt_policy: request.physical_interrupt_policy(),
            interrupt_controller,
            loongarch_platform: self
                .profile
                .loongarch_platform()
                .map(super::LoongArchPlatformProfile::resolve),
            guest_memory: request.memory().to_vec(),
            identity_mappings,
            virtual_devices: resolved_devices,
            host_devices,
            assigned_host_interrupts,
            claims,
        }))
    }
}

fn resolve_assigned_host_interrupts(
    host_devices: &[PlannedHostDevice],
) -> MachinePlanResult<Vec<HostInterruptResource>> {
    let mut routes_by_input = BTreeMap::<u32, (HostDeviceId, HostInterruptResource)>::new();
    let mut assigned_host_interrupts = Vec::new();

    for device in host_devices
        .iter()
        .filter(|device| device.disposition() == DeviceDisposition::Passthrough)
    {
        for interrupt in device.interrupts() {
            let input = interrupt.input_u32();
            if let Some((first_device, first_interrupt)) = routes_by_input.get(&input) {
                if first_interrupt != interrupt {
                    return Err(MachinePlanError::ConflictingHostInterrupt {
                        input,
                        first_device: first_device.to_string(),
                        second_device: device.id().to_string(),
                    });
                }
                continue;
            }

            routes_by_input.insert(input, (device.id().clone(), interrupt.clone()));
            assigned_host_interrupts.push(interrupt.clone());
        }
    }

    Ok(assigned_host_interrupts)
}

fn validate_request(request: &VmMachineRequest) -> MachinePlanResult<()> {
    validate_delivery(request)?;
    if request.vcpu_count() == 0 {
        return Err(MachinePlanError::InvalidVcpuCount);
    }
    validate_unique_instances(request)
}

fn validate_delivery(request: &VmMachineRequest) -> MachinePlanResult<()> {
    if request.mode() == VmMachineMode::Virtual
        && request
            .physical_interrupt_policy()
            .uses_hardware_forwarding()
    {
        return Err(MachinePlanError::HardwareForwardingInVirtualMachine);
    }
    Ok(())
}

fn validate_unique_instances(request: &VmMachineRequest) -> MachinePlanResult<()> {
    let mut instances = BTreeSet::new();
    for device in request.virtual_devices() {
        if !instances.insert(device.instance_id().as_str()) {
            return Err(MachinePlanError::DuplicateDeviceInstance {
                id: device.instance_id().to_string(),
            });
        }
    }
    Ok(())
}

fn resolve_denied_devices(
    request: &VmMachineRequest,
    snapshot: &HostPlatformSnapshot,
) -> MachinePlanResult<BTreeSet<usize>> {
    let mut denied = BTreeSet::new();
    for selector in request.denied() {
        let matches = snapshot
            .devices()
            .iter()
            .enumerate()
            .filter_map(|(index, device)| {
                let selected = match selector {
                    HostDeviceSelector::Mmio(denied_range) => device
                        .mmio()
                        .iter()
                        .any(|resource| resource.overlaps(*denied_range)),
                    HostDeviceSelector::Interrupt(denied_interrupt) => device
                        .interrupts()
                        .iter()
                        .any(|interrupt| interrupt.input() == *denied_interrupt),
                    _ => selector.matches(device),
                };
                selected.then_some(index)
            })
            .collect::<Vec<_>>();
        if matches.is_empty()
            && !matches!(
                selector,
                HostDeviceSelector::Mmio(_) | HostDeviceSelector::Interrupt(_)
            )
        {
            return Err(MachinePlanError::HostSelectorNotFound {
                selector: selector.label(),
            });
        }
        denied.extend(matches);
    }
    Ok(denied)
}

fn select_template(
    mode: VmMachineMode,
    device: &VirtualDeviceDescriptor,
    snapshot: &HostPlatformSnapshot,
    consumed: &BTreeSet<usize>,
) -> MachinePlanResult<Option<usize>> {
    if mode == VmMachineMode::Virtual {
        return match device.source() {
            VirtualDeviceSource::Host(_) => Err(MachinePlanError::HostTemplateInVirtualMachine {
                device: device.instance_id().to_string(),
            }),
            VirtualDeviceSource::Auto | VirtualDeviceSource::Allocate => Ok(None),
        };
    }

    match device.source() {
        VirtualDeviceSource::Allocate => Ok(None),
        VirtualDeviceSource::Auto => Ok(select_automatic_template(device, snapshot, consumed)),
        VirtualDeviceSource::Host(selector) => {
            let Some((index, selected)) = snapshot
                .devices()
                .iter()
                .enumerate()
                .find(|(_, candidate)| selector.matches(candidate))
            else {
                return Err(MachinePlanError::HostSelectorNotFound {
                    selector: selector.label(),
                });
            };
            if consumed.contains(&index) {
                return Err(MachinePlanError::HostTemplateAlreadyUsed {
                    device: selected.id().to_string(),
                });
            }
            Ok(Some(index))
        }
    }
}

fn select_automatic_template(
    device: &VirtualDeviceDescriptor,
    snapshot: &HostPlatformSnapshot,
    consumed: &BTreeSet<usize>,
) -> Option<usize> {
    let matches_device = |index: usize, candidate: &super::HostDeviceDescriptor| {
        !consumed.contains(&index)
            && candidate.compatibles().iter().any(|compatible| {
                device
                    .compatible_predicates()
                    .iter()
                    .any(|accepted| accepted == compatible)
            })
    };
    if matches!(device.backend(), DeviceBackend::HostConsole(_))
        && let Some(console) = snapshot.console_device()
        && let Some((index, _)) =
            snapshot
                .devices()
                .iter()
                .enumerate()
                .find(|(index, candidate)| {
                    candidate.id() == console && matches_device(*index, candidate)
                })
    {
        return Some(index);
    }
    snapshot
        .devices()
        .iter()
        .enumerate()
        .find(|(index, candidate)| matches_device(*index, candidate))
        .map(|(index, _)| index)
}
