//! Resource access granted to one device factory invocation.

use alloc::{sync::Arc, vec::Vec};

use axdevice_base::{DeviceId, HostIrqId, IrqLine, MsiEndpoint};

use crate::{interrupt::*, *};

/// VM-owned services available while a device factory is building a device.
pub struct DeviceBuildContext<'a> {
    resources: PlannedBuildResources<'a>,
    pci_host_topology: Option<&'a Arc<ResolvedPciTopology>>,
    /// Run-scoped deferred-work notification port copied from the runtime.
    work_port: Option<Arc<dyn WorkAccessPort>>,
    /// Identity that the first device of this build will receive at
    /// registration; workers use it to scope their completion signal.
    work_device_id: DeviceId,
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
    pub fn mmio(&mut self, slot: impl AsRef<str>) -> DeviceManagerResult<(u64, u64)> {
        let slot = ResourceSlot::new(slot.as_ref())?;
        self.resources.consume(&slot, ResourceClaimSet::mmio)
    }

    /// Consumes a planned port-I/O slot.
    pub fn pio(&mut self, slot: impl AsRef<str>) -> DeviceManagerResult<(u16, u16)> {
        let slot = ResourceSlot::new(slot.as_ref())?;
        self.resources.consume(&slot, ResourceClaimSet::pio)
    }

    /// Consumes a planned host physical IRQ slot.
    ///
    /// This returns only the immutable identity. Architecture code remains
    /// responsible for claiming and programming its physical IRQ backend.
    pub fn host_irq(&mut self, slot: impl AsRef<str>) -> DeviceManagerResult<HostIrqId> {
        let slot = ResourceSlot::new(slot.as_ref())?;
        self.resources.consume(&slot, ResourceClaimSet::host_irq)
    }

    /// Consumes a planned wired IRQ slot and connects one device source.
    pub fn irq(&mut self, slot: impl AsRef<str>) -> DeviceManagerResult<IrqLine> {
        let slot = ResourceSlot::new(slot.as_ref())?;
        let planned = &mut self.resources;
        let resolved = planned.claims.wired_irq(&slot)?;
        let controller = planned.interrupts.wired_controller(resolved.controller())?;
        let line = controller
            .wired_input(resolved.input(), resolved.trigger())?
            .connect()?;
        let lease = planned.claims.consume(&slot)?;
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
    pub fn msi(&mut self, slot: impl AsRef<str>) -> DeviceManagerResult<MsiEndpoint> {
        let slot = ResourceSlot::new(slot.as_ref())?;
        let range = self.build_msi_range(&slot, true)?;
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
    pub fn msi_range(&mut self, slot: impl AsRef<str>) -> DeviceManagerResult<MsiEndpointRange> {
        let slot = ResourceSlot::new(slot.as_ref())?;
        self.build_msi_range(&slot, false)
    }

    /// Returns the frozen topology when building a PCI host node.
    pub fn pci_host_topology(&self) -> Option<&Arc<ResolvedPciTopology>> {
        self.pci_host_topology
    }

    pub(crate) fn planned(
        interrupts: &'a InterruptRegistry,
        claims: ResourceClaimSet,
        pci_host_topology: Option<&'a Arc<ResolvedPciTopology>>,
    ) -> Self {
        Self {
            pci_host_topology,
            work_port: None,
            work_device_id: DeviceId::new(0),
            resources: PlannedBuildResources {
                interrupts,
                claims,
                retained: PlannedBundleResources::new(),
            },
        }
    }

    /// Injects the run-scoped work port and the device identity that the
    /// first device of this build will be registered under.
    pub(crate) fn with_work_port(
        mut self,
        work_port: Option<Arc<dyn WorkAccessPort>>,
        work_device_id: DeviceId,
    ) -> Self {
        self.work_port = work_port;
        self.work_device_id = work_device_id;
        self
    }

    /// Returns the run-scoped deferred-work notification port, if attached.
    pub fn work_port(&self) -> Option<Arc<dyn WorkAccessPort>> {
        self.work_port.clone()
    }

    /// Returns the identity the first device of this build will receive; a
    /// device worker scopes its completion notification with it.
    pub const fn work_device_id(&self) -> DeviceId {
        self.work_device_id
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

    fn build_msi_range(
        &mut self,
        slot: &ResourceSlot,
        require_single: bool,
    ) -> DeviceManagerResult<MsiEndpointRange> {
        let planned = &mut self.resources;
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

impl PlannedBuildResources<'_> {
    fn consume<T>(
        &mut self,
        slot: &ResourceSlot,
        resolve: fn(&ResourceClaimSet, &ResourceSlot) -> DeviceManagerResult<T>,
    ) -> DeviceManagerResult<T> {
        let resource = resolve(&self.claims, slot)?;
        self.retained.leases.push(self.claims.consume(slot)?);
        Ok(resource)
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
