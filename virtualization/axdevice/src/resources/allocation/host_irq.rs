//! Allocation in the host physical interrupt namespace.

use alloc::{string::ToString, vec::Vec};

use axdevice_base::HostIrqId;

use crate::resources::{pool::*, resolved::*, *};

pub(super) struct HostIrqAllocator<'a> {
    pools: &'a ResourcePools,
    owners: Vec<RangeOwner<usize>>,
}

impl<'a> HostIrqAllocator<'a> {
    pub(super) fn new(pools: &'a ResourcePools) -> Self {
        Self {
            pools,
            owners: pools.reserved_host_irqs().to_vec(),
        }
    }

    pub(super) fn allocate(
        &mut self,
        requester: &str,
        slot: &ResourceSlot,
        request: ResourceRequest<HostIrqId>,
    ) -> Result<ResolvedResource, ResourcePlanningError> {
        let irq = match request {
            ResourceRequest::Auto => self
                .pools
                .auto_host_irqs()
                .iter()
                .flat_map(Clone::clone)
                .find(|irq| !self.occupied(*irq))
                .map(HostIrqId::new)
                .ok_or_else(|| ResourcePlanningError::Exhausted {
                    namespace: ResourceNamespace::HostIrq,
                    requester: requester.into(),
                    slot: slot.clone(),
                })?,
            ResourceRequest::Fixed(irq) => {
                if !self
                    .pools
                    .fixed_host_irqs()
                    .iter()
                    .any(|range| range.contains(&irq.value()))
                {
                    return Err(ResourcePlanningError::FixedNotAllowed {
                        namespace: ResourceNamespace::HostIrq,
                        resource: irq.value().to_string(),
                        requester: requester.into(),
                        slot: slot.clone(),
                    });
                }
                irq
            }
        };
        if let Some(existing) = self
            .owners
            .iter()
            .find(|owner| owner.range.contains(&irq.value()))
        {
            return Err(ResourcePlanningError::Conflict {
                namespace: ResourceNamespace::HostIrq,
                resource: irq.value().to_string(),
                existing_owner: existing.owner.clone(),
                requester: requester.into(),
                detail: "host IRQ ownership is exclusive",
            });
        }
        self.owners.push(RangeOwner {
            range: irq.value()..irq.value() + 1,
            owner: requester.into(),
        });
        Ok(ResolvedResource::HostIrq(irq))
    }

    fn occupied(&self, irq: usize) -> bool {
        self.owners.iter().any(|owner| owner.range.contains(&irq))
    }
}
