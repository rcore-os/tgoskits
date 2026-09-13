//! Deterministic, all-or-nothing VM resource planning.

use alloc::{collections::BTreeMap, format, string::String, sync::Arc, vec::Vec};

use axdevice_base::{ControllerInputId, HostIrqId, InterruptControllerId};

use super::{allocation::*, claim::*, *};
use crate::{DeviceManagerError, DeviceManagerResult};

/// An immutable resource plan for one virtual machine.
#[derive(Debug)]
pub struct VmResourcePlan {
    devices: BTreeMap<String, ResolvedDeviceResources>,
    claims: Arc<ResourceClaimDomain>,
}

impl VmResourcePlan {
    /// Returns the resources assigned to one stable device identifier.
    pub fn resources(&self, device_id: &str) -> DeviceManagerResult<&ResolvedDeviceResources> {
        self.devices
            .get(device_id)
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation: "read VM resource plan",
                resource: format!("planned device {device_id}"),
            })
    }

    /// Returns the number of planned devices.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    /// Iterates in stable device-identifier order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ResolvedDeviceResources)> {
        self.devices
            .iter()
            .map(|(device_id, resources)| (device_id.as_str(), resources))
    }

    /// Issues all one-shot claims for one device atomically.
    pub fn claim_device(&self, device_id: &str) -> DeviceManagerResult<ResourceClaimSet> {
        self.claims.issue_device(device_id)
    }

    /// Verifies that every planned slot is currently held by a lease.
    pub fn verify_consumed(&self) -> DeviceManagerResult {
        self.claims.verify_leased()
    }

    /// Returns the planned owner of a controller input for diagnostics.
    pub fn owner_of_controller_input(
        &self,
        controller: InterruptControllerId,
        input: ControllerInputId,
    ) -> Option<String> {
        self.claims.owner_of_controller_input(controller, input)
    }

    /// Returns the planned owner of a host IRQ for diagnostics.
    pub fn owner_of_host_irq(&self, irq: HostIrqId) -> Option<String> {
        self.claims.owner_of_host_irq(irq)
    }
}

/// Deterministic planner over architecture-provided pools.
pub struct VmResourcePlanner {
    pools: ResourcePools,
}

impl VmResourcePlanner {
    /// Creates a planner for one architecture-owned VM initialization flow.
    pub const fn new(pools: ResourcePools) -> Self {
        Self { pools }
    }

    /// Resolves fixed requests first, then automatic requests lowest-first.
    ///
    /// All allocation state remains local until every request succeeds, so an
    /// error cannot publish a partial plan.
    pub fn plan(
        self,
        requests: impl IntoIterator<Item = DevicePlanRequest>,
    ) -> Result<VmResourcePlan, ResourcePlanningError> {
        let mut requests: Vec<DevicePlanRequest> = requests.into_iter().collect();
        requests.sort_by(|left, right| left.id().cmp(right.id()));
        if let Some(pair) = requests
            .windows(2)
            .find(|pair| pair[0].id() == pair[1].id())
        {
            return Err(ResourcePlanningError::Conflict {
                namespace: ResourceNamespace::Device,
                resource: pair[0].id().into(),
                existing_owner: pair[0].id().into(),
                requester: pair[1].id().into(),
                detail: "stable device identifiers must be unique",
            });
        }

        let mut work = collect_work(&requests);
        work.sort_by(|left, right| {
            (!left.requirement.is_fixed())
                .cmp(&(!right.requirement.is_fixed()))
                .then_with(|| left.device_id.cmp(&right.device_id))
                .then_with(|| left.requirement.slot().cmp(right.requirement.slot()))
        });

        let mut allocations = AllocationState::new(&self.pools);
        let mut devices = requests
            .iter()
            .map(|request| (request.id().into(), ResolvedDeviceResources::default()))
            .collect::<BTreeMap<_, _>>();
        for item in work {
            let resource = allocations.allocate(&item.device_id, &item.requirement)?;
            devices
                .get_mut(&item.device_id)
                .expect("device map and work items originate from the same requests")
                .insert(item.requirement.slot().clone(), resource)
                .expect("requirements reject duplicate slots before planning");
        }

        let claims = ResourceClaimDomain::new(&devices);
        Ok(VmResourcePlan { devices, claims })
    }
}

struct WorkItem {
    device_id: String,
    requirement: DeviceRequirement,
}

fn collect_work(requests: &[DevicePlanRequest]) -> Vec<WorkItem> {
    requests
        .iter()
        .flat_map(|request| {
            request
                .requirements()
                .entries()
                .iter()
                .cloned()
                .map(|requirement| WorkItem {
                    device_id: request.id().into(),
                    requirement,
                })
        })
        .collect()
}
