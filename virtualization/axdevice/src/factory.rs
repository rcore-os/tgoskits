// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Extensible construction of emulated devices from VM configuration.

use alloc::{sync::Arc, vec::Vec};

use axdevice_base::{
    ControllerInputId, InterruptTriggerMode, IrqLine, MsiEndpoint, VirtualInterruptController,
};
use axvm_types::{EmulatedDeviceConfig, EmulatedDeviceType};

use crate::{
    DeviceBundle, DeviceManagerError, DeviceManagerResult, GuestRangeAllocatorKey, ResolvedMsi,
    ResourceClaimSet, ResourceSlot, ServiceCardinality, ServiceKey,
    interrupt::{
        EndpointRegistration, InterruptRegistry, MessageEndpointRegistration,
        PlannedBundleResources, WiredEndpointRegistration,
    },
    range_alloc::IvcGuestRangeAllocator,
};

/// Typed service key for the VM's canonical virtual interrupt controller.
pub struct VirtualInterruptControllerKey;

impl ServiceKey for VirtualInterruptControllerKey {
    type Service = dyn VirtualInterruptController;

    const NAME: &'static str = "virtual-interrupt-controller";
    const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
}

/// VM-owned services available while a device factory is building a device.
pub struct DeviceBuildContext<'a> {
    resources: BuildResources<'a>,
}

enum BuildResources<'a> {
    Legacy(&'a dyn VirtualInterruptController),
    Planned(PlannedBuildResources<'a>),
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
    /// Creates a device build context backed by the VM's canonical controller.
    pub const fn new(interrupt_controller: &'a dyn VirtualInterruptController) -> Self {
        Self {
            resources: BuildResources::Legacy(interrupt_controller),
        }
    }

    /// Returns the VM's canonical virtual interrupt controller.
    pub fn interrupt_controller(&self) -> DeviceManagerResult<&dyn VirtualInterruptController> {
        match &self.resources {
            BuildResources::Legacy(controller) => Ok(*controller),
            BuildResources::Planned(_) => Err(DeviceManagerError::InvalidState {
                operation: "read canonical interrupt controller from device build context",
                detail: "planned devices must select their controller through an IRQ slot".into(),
            }),
        }
    }

    /// Claims a source connection on one VM-local controller input.
    pub fn resolve_irq(
        &self,
        line: usize,
        trigger: InterruptTriggerMode,
    ) -> DeviceManagerResult<IrqLine> {
        Ok(self
            .interrupt_controller()?
            .wired_input(ControllerInputId::new(line), trigger)?
            .connect()?)
    }

    /// Consumes a planned MMIO slot.
    pub fn mmio(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<(u64, u64)> {
        let planned = self.planned_mut("resolve planned MMIO resource")?;
        let lease = planned.claims.consume(slot)?;
        let resource = lease.mmio()?;
        planned.retained.leases.push(lease);
        Ok(resource)
    }

    /// Consumes a planned port-I/O slot.
    pub fn pio(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<(u16, u16)> {
        let planned = self.planned_mut("resolve planned port-I/O resource")?;
        let lease = planned.claims.consume(slot)?;
        let resource = lease.pio()?;
        planned.retained.leases.push(lease);
        Ok(resource)
    }

    /// Consumes a planned wired IRQ slot and connects one device source.
    pub fn irq(&mut self, slot: &ResourceSlot) -> DeviceManagerResult<IrqLine> {
        let planned = self.planned_mut("resolve planned wired IRQ")?;
        let lease = planned.claims.consume(slot)?;
        let resolved = lease.wired_irq()?;
        let controller = planned.interrupts.wired_controller(resolved.controller())?;
        let line = controller
            .wired_input(resolved.input(), resolved.trigger())?
            .connect()?;
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
            resources: BuildResources::Planned(PlannedBuildResources {
                interrupts,
                claims,
                retained: PlannedBundleResources::new(),
            }),
        }
    }

    pub(crate) fn finish(self, mut bundle: DeviceBundle) -> DeviceManagerResult<DeviceBundle> {
        if let BuildResources::Planned(planned) = self.resources {
            planned.claims.finish()?;
            bundle.planned.endpoints.extend(planned.retained.endpoints);
            bundle.planned.leases.extend(planned.retained.leases);
        }
        Ok(bundle)
    }

    fn planned_mut(
        &mut self,
        operation: &'static str,
    ) -> DeviceManagerResult<&mut PlannedBuildResources<'a>> {
        match &mut self.resources {
            BuildResources::Planned(planned) => Ok(planned),
            BuildResources::Legacy(_) => Err(DeviceManagerError::InvalidState {
                operation,
                detail: "the device was not built from a VM resource plan".into(),
            }),
        }
    }

    fn build_msi_range(
        &mut self,
        slot: &ResourceSlot,
        require_single: bool,
    ) -> DeviceManagerResult<MsiEndpointRange> {
        let planned = self.planned_mut("resolve planned MSI endpoint")?;
        let lease = planned.claims.consume(slot)?;
        let resolved = lease.msi()?;
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

/// Builds all capabilities contributed by one emulated device type.
///
/// A factory that exposes an architecture-owned, pre-created controller must
/// capture the same shared controller instance and validate that each build
/// request matches the configuration used to create it, including its MMIO
/// base, length, and type-specific arguments.
pub trait DeviceFactory: Send + Sync {
    /// Returns the configuration type handled by this factory.
    fn device_type(&self) -> EmulatedDeviceType;

    /// Builds a device without modifying the destination device registry.
    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle>;
}

/// A registry containing at most one factory for each emulated device type.
///
/// Registered factories are authoritative for their device type: during
/// [`DeviceRuntime::build_with_factories`](crate::DeviceRuntime::build_with_factories),
/// each configured device has exactly one construction path. A factory error
/// is propagated and never causes a fallback to create another device.
///
/// Architectures that pre-create an interrupt controller must first reject
/// duplicate controller configurations, then register exactly one factory that
/// captures the shared controller and its validated configuration fingerprint.
#[derive(Default)]
pub struct DeviceFactoryRegistry {
    factories: Vec<(EmulatedDeviceType, Arc<dyn DeviceFactory>)>,
}

impl DeviceFactoryRegistry {
    /// Creates an empty factory registry.
    pub const fn new() -> Self {
        Self {
            factories: Vec::new(),
        }
    }

    /// Registers a factory, rejecting a duplicate device type.
    pub fn register(&mut self, factory: Arc<dyn DeviceFactory>) -> DeviceManagerResult {
        let device_type = factory.device_type();
        if self.get(device_type).is_some() {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "register device factory",
                detail: alloc::format!(
                    "factory for device type {device_type} is already registered"
                ),
            });
        }
        self.factories.push((device_type, factory));
        Ok(())
    }

    /// Returns the factory registered for `device_type`.
    pub fn get(&self, device_type: EmulatedDeviceType) -> Option<&dyn DeviceFactory> {
        self.factories
            .iter()
            .find(|(registered_type, _)| *registered_type == device_type)
            .map(|(_, factory)| factory.as_ref())
    }

    /// Builds a bundle for `config`.
    pub fn build(
        &self,
        config: &EmulatedDeviceConfig,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let Some(factory) = self.get(config.emu_type) else {
            return Err(DeviceManagerError::Unsupported {
                operation: "build emulated device",
                detail: alloc::format!(
                    "no factory is registered for emulated device '{}' of type {}",
                    config.name,
                    config.emu_type
                ),
            });
        };
        factory.build(config, context)
    }
}

struct MetaDeviceFactory;

struct IvcChannelFactory;

impl DeviceFactory for IvcChannelFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::IVCChannel
    }

    fn build(
        &self,
        config: &EmulatedDeviceConfig,
        _context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let allocator = IvcGuestRangeAllocator::new(config.base_gpa, config.length)?.into_service();
        DeviceBundle::new().with_service::<GuestRangeAllocatorKey>(allocator)
    }
}

impl DeviceFactory for MetaDeviceFactory {
    fn device_type(&self) -> EmulatedDeviceType {
        EmulatedDeviceType::Dummy
    }

    fn build(
        &self,
        _config: &EmulatedDeviceConfig,
        _context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        Ok(DeviceBundle::new())
    }
}

/// Registers device factories that do not depend on an architecture backend.
pub fn register_builtin_factories(registry: &mut DeviceFactoryRegistry) -> DeviceManagerResult {
    registry.register(Arc::new(MetaDeviceFactory))?;
    registry.register(Arc::new(IvcChannelFactory))
}
