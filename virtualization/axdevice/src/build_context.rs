//! Resource access granted to one device factory invocation.

use alloc::vec::Vec;

use axdevice_base::{HostIrqId, IrqLine, MsiEndpoint};

use crate::{
    DeviceBundle, DeviceManagerError, DeviceManagerResult, ResolvedMsi, ResourceClaimSet,
    ResourceSlot,
    interrupt::{
        EndpointRegistration, InterruptRegistry, MessageEndpointRegistration,
        PlannedBundleResources, WiredEndpointRegistration,
    },
};

/// VM-owned services available while a device factory is building a device.
pub struct DeviceBuildContext<'a> {
    resources: PlannedBuildResources<'a>,
}

struct PlannedBuildResources<'a> {
    interrupts: &'a InterruptRegistry,
    claims: ResourceClaimSet,
    retained: PlannedBundleResources,
}

/// A contiguous set of planner-authorized MSI endpoints.
pub struct MsiEndpointRange {
    resolved: ResolvedMsi,
    endpoints: Vec<MsiEndpoint>,
}

impl<'a> DeviceBuildContext<'a> {
    /// Consumes a planned MMIO slot.
    pub fn mmio(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<(u64, u64)> {
        let planned = self.planned_mut("resolve planned MMIO resource")?;
        let resource = planned.claims.mmio(slot)?;
        let lease = planned.claims.consume(slot)?;
        planned.retained.leases.push(lease);
        Ok(resource)
    }

    /// Consumes a planned port-I/O slot.
    pub fn pio(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<(u16, u16)> {
        let planned = self.planned_mut("resolve planned port-I/O resource")?;
        let resource = planned.claims.pio(slot)?;
        let lease = planned.claims.consume(slot)?;
        planned.retained.leases.push(lease);
        Ok(resource)
    }

    /// Consumes a planned host physical IRQ slot.
    ///
    /// This returns only the immutable identity. Architecture code remains
    /// responsible for claiming and programming its physical IRQ backend.
    pub fn host_irq(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<HostIrqId> {
        let planned = self.planned_mut("resolve planned host IRQ")?;
        let resource = planned.claims.host_irq(slot)?;
        let lease = planned.claims.consume(slot)?;
        planned.retained.leases.push(lease);
        Ok(resource)
    }

    /// Consumes a planned wired IRQ slot and connects one device source.
    pub fn irq(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<IrqLine> {
        let planned = self.planned_mut("resolve planned wired IRQ")?;
        let resolved = planned.claims.wired_irq(slot)?;
        let controller = planned.interrupts.wired_controller(resolved.controller())?;
        let line = controller
            .wired_input(resolved.input(), resolved.trigger())?
            .connect()?;
        let lease = planned.claims.consume(slot)?;
        planned
            .retained
            .endpoints
            .push(EndpointRegistration::Wired(WiredEndpointRegistration {
                resolved,
                lease,
            }));
        Ok(line)
    }

    /// Consumes a single-message MSI slot.
    pub fn msi(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<MsiEndpoint> {
        let range = self.build_msi_range(slot, true)?;
        range
            .endpoints
            .into_iter()
            .next()
            .ok_or_else(|| DeviceManagerError::InvalidState {
                operation: "resolve planned MSI endpoint",
                detail: "single-message MSI range was empty".into(),
            })
    }

    /// Consumes a contiguous MSI event/LPI range.
    pub fn msi_range(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<MsiEndpointRange> {
        self.build_msi_range(slot, false)
    }

    pub(crate) fn planned(interrupts: &'a InterruptRegistry, claims: ResourceClaimSet) -> Self {
        Self {
            resources: PlannedBuildResources {
                interrupts,
                claims,
                retained: PlannedBundleResources::new(),
            },
        }
    }

    pub(crate) fn finish(self, mut bundle: DeviceBundle) -> DeviceManagerResult<DeviceBundle> {
        self.resources.claims.finish()?;
        bundle
            .planned
            .endpoints
            .extend(self.resources.retained.endpoints);
        bundle.planned.leases.extend(self.resources.retained.leases);
        Ok(bundle)
    }

    fn planned_mut(
        &mut self,
        _operation: &'static str,
    ) -> DeviceManagerResult<&mut PlannedBuildResources<'a>> {
        Ok(&mut self.resources)
    }

    fn build_msi_range(
        &mut self,
        slot: &ResourceSlot,
        require_single: bool,
    ) -> DeviceManagerResult<MsiEndpointRange> {
        let planned = self.planned_mut("resolve planned MSI endpoint")?;
        let resolved = planned.claims.msi(slot)?;
        if require_single && resolved.count() != 1 {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "resolve planned MSI endpoint",
                detail: alloc::format!(
                    "slot {slot} contains {} messages; use msi_range()",
                    resolved.count()
                ),
            });
        }
        let controller = planned
            .interrupts
            .message_controller(resolved.controller())?;
        let mut endpoints = Vec::with_capacity(resolved.count() as usize);
        for offset in 0..resolved.count() {
            let event = resolved
                .event()
                .value()
                .checked_add(offset)
                .map(axdevice_base::MsiEventId::new)
                .ok_or_else(|| invalid_msi_range(slot))?;
            let lpi = resolved
                .lpi()
                .value()
                .checked_add(offset)
                .map(axdevice_base::LpiId::new)
                .ok_or_else(|| invalid_msi_range(slot))?;
            endpoints.push(controller.msi_endpoint(
                resolved.its(),
                resolved.device(),
                event,
                lpi,
            )?);
        }
        let lease = planned.claims.consume(slot)?;
        planned
            .retained
            .endpoints
            .push(EndpointRegistration::Message(MessageEndpointRegistration {
                resolved,
                lease,
            }));
        Ok(MsiEndpointRange {
            resolved,
            endpoints,
        })
    }
}

impl MsiEndpointRange {
    /// Returns the planner-resolved MSI identity range.
    pub const fn resolved(&self) -> ResolvedMsi {
        self.resolved
    }

    /// Returns all endpoints in EventID/LPI order.
    pub fn endpoints(&self) -> &[MsiEndpoint] {
        &self.endpoints
    }

    /// Transfers the endpoints to the device implementation.
    pub fn into_endpoints(self) -> Vec<MsiEndpoint> {
        self.endpoints
    }
}

fn invalid_msi_range(slot: &ResourceSlot) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation: "resolve planned MSI endpoint",
        detail: alloc::format!("slot {slot} overflows its EventID or LPI range"),
    }
}
