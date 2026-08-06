//! MMIO and port-I/O allocation.

use alloc::{format, string::String, vec::Vec};

use super::search::*;
use crate::resources::{pool::*, resolved::*, *};

pub(super) struct AddressAllocator<'a> {
    pools: &'a ResourcePools,
    allocated: AddressAllocations,
}

#[derive(Default)]
struct AddressAllocations {
    mmio: Vec<RangeOwner<u64>>,
    pio: Vec<RangeOwner<u16>>,
}

impl<'a> AddressAllocator<'a> {
    pub(super) fn new(pools: &'a ResourcePools) -> Self {
        Self {
            pools,
            allocated: AddressAllocations {
                mmio: pools.reserved_mmio().to_vec(),
                pio: pools.reserved_pio().to_vec(),
            },
        }
    }

    pub(super) fn allocate_mmio(
        &mut self,
        requester: &str,
        slot: &ResourceSlot,
        size: u64,
        alignment: u64,
        request: ResourceRequest<u64>,
    ) -> Result<ResolvedResource, ResourcePlanningError> {
        let base = match request {
            ResourceRequest::Auto => find_u64_range(
                self.pools.auto_mmio(),
                &self.allocated.mmio,
                size,
                alignment,
            )
            .ok_or_else(|| exhausted(ResourceNamespace::Mmio, requester, slot))?,
            ResourceRequest::Fixed(base) => {
                if base % alignment != 0
                    || !range_allowed(Some(self.pools.fixed_mmio()), base, size)
                {
                    return Err(fixed_not_allowed(
                        ResourceNamespace::Mmio,
                        format!("{base:#x}+{size:#x}"),
                        requester,
                        slot,
                    ));
                }
                base
            }
        };
        let end = base.checked_add(size).ok_or_else(|| {
            fixed_not_allowed(
                ResourceNamespace::Mmio,
                format!("{base:#x}+{size:#x}"),
                requester,
                slot,
            )
        })?;
        reserve_allocated_range(
            &mut self.allocated.mmio,
            base,
            end,
            requester,
            ResourceNamespace::Mmio,
        )?;
        Ok(ResolvedResource::Mmio { base, size })
    }

    pub(super) fn allocate_pio(
        &mut self,
        requester: &str,
        slot: &ResourceSlot,
        size: u16,
        alignment: u16,
        request: ResourceRequest<u16>,
    ) -> Result<ResolvedResource, ResourcePlanningError> {
        let base = match request {
            ResourceRequest::Auto => {
                find_u16_range(self.pools.auto_pio(), &self.allocated.pio, size, alignment)
                    .ok_or_else(|| exhausted(ResourceNamespace::Pio, requester, slot))?
            }
            ResourceRequest::Fixed(base) => {
                if base % alignment != 0 || !range_allowed(Some(self.pools.fixed_pio()), base, size)
                {
                    return Err(fixed_not_allowed(
                        ResourceNamespace::Pio,
                        format!("{base:#x}+{size:#x}"),
                        requester,
                        slot,
                    ));
                }
                base
            }
        };
        let end = base.checked_add(size).ok_or_else(|| {
            fixed_not_allowed(
                ResourceNamespace::Pio,
                format!("{base:#x}+{size:#x}"),
                requester,
                slot,
            )
        })?;
        reserve_allocated_range(
            &mut self.allocated.pio,
            base,
            end,
            requester,
            ResourceNamespace::Pio,
        )?;
        Ok(ResolvedResource::Pio { base, size })
    }
}

fn reserve_allocated_range<T>(
    occupied: &mut Vec<RangeOwner<T>>,
    start: T,
    end: T,
    requester: &str,
    namespace: ResourceNamespace,
) -> Result<(), ResourcePlanningError>
where
    T: Copy + Ord + core::fmt::LowerHex,
{
    if let Some(existing) = occupied
        .iter()
        .find(|existing| ranges_overlap(start, end, existing.range.start, existing.range.end))
    {
        return Err(ResourcePlanningError::Conflict {
            namespace,
            resource: format!("{start:#x}..{end:#x}"),
            existing_owner: existing.owner.clone(),
            requester: requester.into(),
            detail: "address ranges overlap",
        });
    }
    occupied.push(RangeOwner {
        range: start..end,
        owner: requester.into(),
    });
    Ok(())
}

fn exhausted(
    namespace: ResourceNamespace,
    requester: &str,
    slot: &ResourceSlot,
) -> ResourcePlanningError {
    ResourcePlanningError::Exhausted {
        namespace,
        requester: requester.into(),
        slot: slot.clone(),
    }
}

fn fixed_not_allowed(
    namespace: ResourceNamespace,
    resource: String,
    requester: &str,
    slot: &ResourceSlot,
) -> ResourcePlanningError {
    ResourcePlanningError::FixedNotAllowed {
        namespace,
        resource,
        requester: requester.into(),
        slot: slot.clone(),
    }
}
