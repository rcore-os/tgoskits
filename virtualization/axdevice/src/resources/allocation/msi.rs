//! ITS DeviceID/EventID and controller-global LPI allocation.

use alloc::{collections::BTreeMap, format, vec::Vec};

use axdevice_base::*;

use super::search::{find_u32_range, range_allowed};
use crate::resources::{error::*, pool::*, resolved::*, *};

pub(super) struct MsiAllocator<'a> {
    pools: &'a ResourcePools,
    allocated: MsiAllocations,
}

#[derive(Default)]
struct MsiAllocations {
    devices: BTreeMap<(InterruptControllerId, ItsId), Vec<RangeOwner<u32>>>,
    events: BTreeMap<(InterruptControllerId, ItsId, MsiDeviceId), Vec<RangeOwner<u32>>>,
    lpis: BTreeMap<InterruptControllerId, Vec<RangeOwner<u32>>>,
}

#[derive(Clone, Copy)]
struct MsiSelection {
    device: MsiDeviceId,
    event: MsiEventId,
    lpi: LpiId,
    count: u32,
}

impl<'a> MsiAllocator<'a> {
    pub(super) fn new(pools: &'a ResourcePools) -> Self {
        Self {
            pools,
            allocated: MsiAllocations {
                devices: pools.reserved_msi_devices().clone(),
                events: pools.reserved_msi_events().clone(),
                lpis: pools.reserved_lpis().clone(),
            },
        }
    }

    pub(super) fn allocate(
        &mut self,
        requester: &str,
        slot: &ResourceSlot,
        request: MsiResourceRequest,
    ) -> Result<ResolvedResource, ResourcePlanningError> {
        let controller = request.controller();
        let its = request.its();
        let count = request.count();
        let domain = (controller, its);

        let device = self.select_device(requester, slot, domain, request.device())?;
        let selection = MsiSelection {
            device,
            event: self.select_event(requester, slot, domain, device, count, request.event())?,
            lpi: self.select_lpi(requester, slot, controller, count, request.lpi())?,
            count,
        };

        self.reserve_selection(requester, slot, domain, selection)?;
        Ok(ResolvedResource::Msi(ResolvedMsi::new(
            controller,
            its,
            selection.device,
            selection.event,
            selection.lpi,
            selection.count,
        )))
    }

    fn select_device(
        &self,
        requester: &str,
        slot: &ResourceSlot,
        domain: (InterruptControllerId, ItsId),
        request: ResourceRequest<MsiDeviceId>,
    ) -> Result<MsiDeviceId, ResourcePlanningError> {
        let ranges = match request {
            ResourceRequest::Auto => self.pools.auto_msi().devices.get(&domain),
            ResourceRequest::Fixed(_) => self.pools.fixed_msi().devices.get(&domain),
        };
        let occupied = self.allocated.devices.get(&domain).map(Vec::as_slice);
        match request {
            ResourceRequest::Auto => find_u32_range(
                ranges.map(Vec::as_slice).unwrap_or_default(),
                occupied.unwrap_or_default(),
                1,
            )
            .map(MsiDeviceId::new)
            .ok_or_else(|| {
                exhausted(
                    ResourceNamespace::Its {
                        controller: domain.0,
                        its: domain.1,
                    },
                    requester,
                    slot,
                )
            }),
            ResourceRequest::Fixed(device) => {
                require_fixed_range(
                    ranges.map(Vec::as_slice),
                    device.value(),
                    1,
                    ResourceNamespace::Its {
                        controller: domain.0,
                        its: domain.1,
                    },
                    format!("device:{}", device.value()),
                    requester,
                    slot,
                )?;
                if let Some(existing) = occupied.and_then(|owners| {
                    owners
                        .iter()
                        .find(|owner| owner.range.contains(&device.value()))
                }) {
                    return Err(conflict(
                        ResourceNamespace::Its {
                            controller: domain.0,
                            its: domain.1,
                        },
                        format!("device:{}", device.value()),
                        &existing.owner,
                        requester,
                        "MSI DeviceIDs are exclusive within one ITS",
                    ));
                }
                Ok(device)
            }
        }
    }

    fn select_event(
        &self,
        requester: &str,
        slot: &ResourceSlot,
        domain: (InterruptControllerId, ItsId),
        device: MsiDeviceId,
        count: u32,
        request: ResourceRequest<MsiEventId>,
    ) -> Result<MsiEventId, ResourcePlanningError> {
        let ranges = match request {
            ResourceRequest::Auto => self.pools.auto_msi().events.get(&domain),
            ResourceRequest::Fixed(_) => self.pools.fixed_msi().events.get(&domain),
        };
        let occupied = self
            .allocated
            .events
            .get(&(domain.0, domain.1, device))
            .map(Vec::as_slice);
        match request {
            ResourceRequest::Auto => find_u32_range(
                ranges.map(Vec::as_slice).unwrap_or_default(),
                occupied.unwrap_or_default(),
                count,
            )
            .map(MsiEventId::new)
            .ok_or_else(|| {
                exhausted(
                    ResourceNamespace::Its {
                        controller: domain.0,
                        its: domain.1,
                    },
                    requester,
                    slot,
                )
            }),
            ResourceRequest::Fixed(event) => {
                require_fixed_range(
                    ranges.map(Vec::as_slice),
                    event.value(),
                    count,
                    ResourceNamespace::Its {
                        controller: domain.0,
                        its: domain.1,
                    },
                    format!("device:{} event:{}+{count}", device.value(), event.value()),
                    requester,
                    slot,
                )?;
                if let Some(existing) = overlapping_owner(occupied, event.value(), count) {
                    return Err(conflict(
                        ResourceNamespace::Its {
                            controller: domain.0,
                            its: domain.1,
                        },
                        format!("device:{} event:{}+{count}", device.value(), event.value()),
                        &existing.owner,
                        requester,
                        "MSI EventID ranges overlap",
                    ));
                }
                Ok(event)
            }
        }
    }

    fn select_lpi(
        &self,
        requester: &str,
        slot: &ResourceSlot,
        controller: InterruptControllerId,
        count: u32,
        request: ResourceRequest<LpiId>,
    ) -> Result<LpiId, ResourcePlanningError> {
        let ranges = match request {
            ResourceRequest::Auto => self.pools.auto_msi().lpis.get(&controller),
            ResourceRequest::Fixed(_) => self.pools.fixed_msi().lpis.get(&controller),
        };
        let occupied = self.allocated.lpis.get(&controller).map(Vec::as_slice);
        match request {
            ResourceRequest::Auto => find_u32_range(
                ranges.map(Vec::as_slice).unwrap_or_default(),
                occupied.unwrap_or_default(),
                count,
            )
            .map(LpiId::new)
            .ok_or_else(|| exhausted(ResourceNamespace::Lpi(controller), requester, slot)),
            ResourceRequest::Fixed(lpi) => {
                require_fixed_range(
                    ranges.map(Vec::as_slice),
                    lpi.value(),
                    count,
                    ResourceNamespace::Lpi(controller),
                    format!("{}+{count}", lpi.value()),
                    requester,
                    slot,
                )?;
                if let Some(existing) = overlapping_owner(occupied, lpi.value(), count) {
                    return Err(conflict(
                        ResourceNamespace::Lpi(controller),
                        format!("{}+{count}", lpi.value()),
                        &existing.owner,
                        requester,
                        "LPI ranges overlap",
                    ));
                }
                Ok(lpi)
            }
        }
    }

    fn reserve_selection(
        &mut self,
        requester: &str,
        slot: &ResourceSlot,
        domain: (InterruptControllerId, ItsId),
        selection: MsiSelection,
    ) -> Result<(), ResourcePlanningError> {
        let device_end = selection.device.value().checked_add(1).ok_or_else(|| {
            invalid_fixed(
                ResourceNamespace::Its {
                    controller: domain.0,
                    its: domain.1,
                },
                format!("device:{}", selection.device.value()),
                requester,
                slot,
            )
        })?;
        let event_end = selection
            .event
            .value()
            .checked_add(selection.count)
            .ok_or_else(|| {
                invalid_fixed(
                    ResourceNamespace::Its {
                        controller: domain.0,
                        its: domain.1,
                    },
                    format!("event:{}+{}", selection.event.value(), selection.count),
                    requester,
                    slot,
                )
            })?;
        let lpi_end = selection
            .lpi
            .value()
            .checked_add(selection.count)
            .ok_or_else(|| {
                invalid_fixed(
                    ResourceNamespace::Lpi(domain.0),
                    format!("{}+{}", selection.lpi.value(), selection.count),
                    requester,
                    slot,
                )
            })?;

        self.allocated
            .devices
            .entry(domain)
            .or_default()
            .push(RangeOwner {
                range: selection.device.value()..device_end,
                owner: requester.into(),
            });
        self.allocated
            .events
            .entry((domain.0, domain.1, selection.device))
            .or_default()
            .push(RangeOwner {
                range: selection.event.value()..event_end,
                owner: requester.into(),
            });
        self.allocated
            .lpis
            .entry(domain.0)
            .or_default()
            .push(RangeOwner {
                range: selection.lpi.value()..lpi_end,
                owner: requester.into(),
            });
        Ok(())
    }
}

fn overlapping_owner(
    occupied: Option<&[RangeOwner<u32>]>,
    start: u32,
    size: u32,
) -> Option<&RangeOwner<u32>> {
    let end = start.checked_add(size)?;
    occupied?
        .iter()
        .find(|owner| ranges_overlap(start, end, owner.range.start, owner.range.end))
}

fn require_fixed_range(
    pools: Option<&[core::ops::Range<u32>]>,
    start: u32,
    size: u32,
    namespace: ResourceNamespace,
    resource: alloc::string::String,
    requester: &str,
    slot: &ResourceSlot,
) -> Result<(), ResourcePlanningError> {
    if range_allowed(pools, start, size) {
        return Ok(());
    }
    Err(invalid_fixed(namespace, resource, requester, slot))
}

fn invalid_fixed(
    namespace: ResourceNamespace,
    resource: alloc::string::String,
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
