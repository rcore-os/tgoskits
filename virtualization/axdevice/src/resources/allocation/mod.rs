//! Allocation orchestration over independent resource namespaces.

mod address;
mod host_irq;
mod msi;
mod search;
mod wired;

use address::AddressAllocator;
use host_irq::HostIrqAllocator;
use msi::MsiAllocator;
use wired::WiredAllocator;

use super::{resolved::*, *};

pub(super) struct AllocationState<'a> {
    addresses: AddressAllocator<'a>,
    wired: WiredAllocator<'a>,
    host_irqs: HostIrqAllocator<'a>,
    msi: MsiAllocator<'a>,
}

impl<'a> AllocationState<'a> {
    pub(super) fn new(pools: &'a ResourcePools) -> Self {
        Self {
            addresses: AddressAllocator::new(pools),
            wired: WiredAllocator::new(pools),
            host_irqs: HostIrqAllocator::new(pools),
            msi: MsiAllocator::new(pools),
        }
    }

    pub(super) fn allocate(
        &mut self,
        requester: &str,
        requirement: &DeviceRequirement,
    ) -> Result<ResolvedResource, ResourcePlanningError> {
        match requirement {
            DeviceRequirement::Mmio {
                slot,
                size,
                alignment,
                request,
            } => self
                .addresses
                .allocate_mmio(requester, slot, *size, *alignment, *request),
            DeviceRequirement::Pio {
                slot,
                size,
                alignment,
                request,
            } => self
                .addresses
                .allocate_pio(requester, slot, *size, *alignment, *request),
            DeviceRequirement::WiredIrq {
                slot,
                controller,
                trigger,
                sharing,
                request,
            } => self
                .wired
                .allocate(requester, slot, *controller, *trigger, *sharing, *request),
            DeviceRequirement::HostIrq { slot, request } => {
                self.host_irqs.allocate(requester, slot, *request)
            }
            DeviceRequirement::Msi { slot, request } => {
                self.msi.allocate(requester, slot, *request)
            }
        }
    }
}
