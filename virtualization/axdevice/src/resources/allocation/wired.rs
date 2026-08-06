//! Wired controller-input allocation and sharing validation.

use alloc::{collections::BTreeMap, string::ToString, vec::Vec};

use axdevice_base::*;

use crate::resources::{pool::*, resolved::*, *};

pub(super) struct WiredAllocator<'a> {
    pools: &'a ResourcePools,
    owners: BTreeMap<InterruptControllerId, Vec<IrqOwner>>,
}

impl<'a> WiredAllocator<'a> {
    pub(super) fn new(pools: &'a ResourcePools) -> Self {
        Self {
            pools,
            owners: pools.reserved_inputs().clone(),
        }
    }

    pub(super) fn allocate(
        &mut self,
        requester: &str,
        slot: &ResourceSlot,
        controller: InterruptControllerId,
        trigger: InterruptTrigger,
        sharing: InterruptSharing,
        request: ResourceRequest<ControllerInputId>,
    ) -> Result<ResolvedResource, ResourcePlanningError> {
        let input = match request {
            ResourceRequest::Auto => self.find_free_input(controller).ok_or_else(|| {
                ResourcePlanningError::Exhausted {
                    namespace: ResourceNamespace::ControllerInput(controller),
                    requester: requester.into(),
                    slot: slot.clone(),
                }
            })?,
            ResourceRequest::Fixed(input) => {
                if !self.fixed_input_allowed(controller, input) {
                    return Err(ResourcePlanningError::FixedNotAllowed {
                        namespace: ResourceNamespace::ControllerInput(controller),
                        resource: input.value().to_string(),
                        requester: requester.into(),
                        slot: slot.clone(),
                    });
                }
                input
            }
        };

        let owners = self.owners.entry(controller).or_default();
        if let Some(existing) = owners.iter().find(|owner| owner.input == input) {
            validate_sharing(existing, requester, controller, input, trigger, sharing)?;
        }
        owners.push(IrqOwner {
            input,
            trigger,
            sharing,
            owner: requester.into(),
        });
        Ok(ResolvedResource::WiredIrq(ResolvedWiredIrq::new(
            controller, input, trigger, sharing,
        )))
    }

    fn find_free_input(&self, controller: InterruptControllerId) -> Option<ControllerInputId> {
        let occupied = self.owners.get(&controller);
        self.pools
            .auto_inputs(controller)?
            .iter()
            .flat_map(Clone::clone)
            .find(|input| {
                !occupied
                    .is_some_and(|owners| owners.iter().any(|owner| owner.input.value() == *input))
            })
            .map(ControllerInputId::new)
    }

    fn fixed_input_allowed(
        &self,
        controller: InterruptControllerId,
        input: ControllerInputId,
    ) -> bool {
        self.pools
            .fixed_inputs(controller)
            .is_some_and(|ranges| ranges.iter().any(|range| range.contains(&input.value())))
    }
}

fn validate_sharing(
    existing: &IrqOwner,
    requester: &str,
    controller: InterruptControllerId,
    input: ControllerInputId,
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
) -> Result<(), ResourcePlanningError> {
    let compatible = sharing == InterruptSharing::Shared
        && existing.sharing == InterruptSharing::Shared
        && existing.trigger == trigger;
    if compatible {
        return Ok(());
    }
    let detail = if existing.trigger != trigger {
        "shared inputs require identical trigger semantics"
    } else {
        "an exclusive request cannot share a controller input"
    };
    Err(ResourcePlanningError::Conflict {
        namespace: ResourceNamespace::ControllerInput(controller),
        resource: input.value().to_string(),
        existing_owner: existing.owner.clone(),
        requester: requester.into(),
        detail,
    })
}
