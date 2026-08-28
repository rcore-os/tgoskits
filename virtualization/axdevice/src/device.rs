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

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use axdevice_base::*;
use axvm_types::GuestPhysAddr;

use crate::{runtime_resources::*, *};

/// Runtime backend for access-scoped virtual timer requests.
pub trait TimerAccessPort: Send + Sync {
    /// Schedules a VM-local timer deadline for `device_id`.
    fn schedule_timer(&self, device_id: DeviceId, deadline_ns: u64) -> DeviceManagerResult;
}

/// Runtime backend for access-scoped vCPU wake requests.
pub trait WakeAccessPort: Send + Sync {
    /// Wakes a VM-local vCPU on behalf of `device_id`.
    fn wake_vcpu(&self, device_id: DeviceId, vcpu_id: usize) -> DeviceManagerResult;
}

/// Runtime backend for access-scoped VM stop requests.
pub trait StopAccessPort: Send + Sync {
    /// Requests a VM stop on behalf of `device_id`.
    fn request_vm_stop(&self, device_id: DeviceId, reason: &str) -> DeviceManagerResult;
}

/// VM runtime capabilities injected into one sealed [`DeviceRuntime`].
#[derive(Clone, Default)]
pub struct RuntimeAccessPorts {
    timer: Option<Arc<dyn TimerAccessPort>>,
    wake: Option<Arc<dyn WakeAccessPort>>,
    stop: Option<Arc<dyn StopAccessPort>>,
}

impl RuntimeAccessPorts {
    /// Creates an empty access-port set.
    pub const fn new() -> Self {
        Self {
            timer: None,
            wake: None,
            stop: None,
        }
    }

    /// Adds a timer scheduling port.
    pub fn with_timer(mut self, timer: Arc<dyn TimerAccessPort>) -> Self {
        self.timer = Some(timer);
        self
    }

    /// Adds a vCPU wake port.
    pub fn with_wake(mut self, wake: Arc<dyn WakeAccessPort>) -> Self {
        self.wake = Some(wake);
        self
    }

    /// Adds a VM stop-request port.
    pub fn with_stop(mut self, stop: Arc<dyn StopAccessPort>) -> Self {
        self.stop = Some(stop);
        self
    }
}

#[inline]
#[allow(dead_code)]
fn log_device_io(
    addr_type: &'static str,
    addr: impl core::fmt::LowerHex,
    addr_range: impl core::fmt::LowerHex,
    read: bool,
    width: AccessWidth,
) {
    let rw = if read { "read" } else { "write" };
    trace!("emu_device {rw}: {addr_type} {addr:#x} in range {addr_range:#x} with width {width:?}")
}

/// Internal range entry cached in the index maps.
struct RangeEntry {
    slot: usize,
    size: u64,
}

fn ranges_overlap(start: u64, end: u64, other_start: u64, other_end: u64) -> bool {
    start < other_end && other_start < end
}

fn range_contains_access(base: u64, size: u64, addr: u64, width: AccessWidth) -> bool {
    let Some(resource_end) = base.checked_add(size) else {
        return false;
    };
    let Some(access_end) = addr.checked_add(width.size() as u64) else {
        return false;
    };

    base <= addr && access_end <= resource_end
}

fn validate_bundle_grant_indices<T>(
    device_count: usize,
    grants: &[(usize, T)],
    operation: &'static str,
    capability_name: &'static str,
) -> DeviceManagerResult {
    if grants.iter().any(|(index, _)| *index >= device_count)
        || grants.iter().enumerate().any(|(position, (index, _))| {
            grants[..position]
                .iter()
                .any(|(existing, _)| existing == index)
        })
    {
        return Err(DeviceManagerError::InvalidConfig {
            operation,
            detail: alloc::format!("{capability_name} must name each bundled device at most once"),
        });
    }
    Ok(())
}

/// Per-VM runtime that owns the static emulated-device topology.
///
/// Construction mutates the registry through [`DeviceRegistry`]. Once shared
/// with vCPUs, routing is read-only and every access enters through
/// [`DeviceRuntime::try_read`] or [`DeviceRuntime::try_write`]. Production
/// construction always uses a factory and an atomic [`DeviceBundle`]
/// registration.
pub struct DeviceRuntime {
    /// Registered devices (append-only; index is the DeviceId).
    devices: Vec<Arc<dyn Device>>,
    /// MMIO base address → range entry (slot, size).
    mmio_index: BTreeMap<u64, RangeEntry>,
    /// Port I/O base address → range entry (slot, size).
    port_index: BTreeMap<u16, RangeEntry>,
    /// System register address → range entry (slot, count).
    sysreg_index: BTreeMap<u32, RangeEntry>,
    /// Devices that require periodic polling.
    pollable_devices: Vec<Arc<dyn PollableDeviceOps>>,
    /// Devices whose periodic progress requires scoped guest-memory DMA.
    dma_pollable_devices: Vec<(DeviceId, Arc<dyn DmaPollableDeviceOps>, DmaGrant)>,
    /// Optional lifecycle capabilities in contribution registration order.
    lifecycle_devices: Vec<Arc<dyn DeviceLifecycle>>,
    /// Typed capabilities contributed during VM preparation.
    services: DeviceServices,
    /// Planned controller, endpoint, and lease state.
    planned: PlannedRuntimeResources,
    /// Devices explicitly granted access to guest memory during a routed access.
    ///
    /// The grant is intentionally narrow: the VM supplies a guest-memory port
    /// only for the duration of one device write or DMA poll callback.
    dma_grants: Vec<(DeviceId, DmaGrant)>,
    /// Devices explicitly granted timer scheduling during a routed access.
    timer_grants: Vec<(DeviceId, TimerGrant)>,
    /// Devices explicitly granted vCPU wake access during a routed access.
    wake_grants: Vec<(DeviceId, WakeGrant)>,
    /// Devices explicitly granted VM stop-request access during a routed access.
    stop_grants: Vec<(DeviceId, StopGrant)>,
    /// VM runtime access ports used after grant verification.
    access_ports: RuntimeAccessPorts,
    /// Whether this runtime topology has been frozen after VM preparation.
    sealed: bool,
    pci_roots: BTreeMap<DeviceNodeId, Arc<PciRootBinding>>,
    pci_binding_leases: Vec<crate::pci::PciBindingLease>,
}

/// Stack-scoped metadata for one routed device access.
struct RuntimeDeviceContext<'runtime, 'memory> {
    device_id: DeviceId,
    memory: Option<&'memory mut dyn GuestMemoryAccess>,
    dma_grants: &'runtime [(DeviceId, DmaGrant)],
    timer_grants: &'runtime [(DeviceId, TimerGrant)],
    wake_grants: &'runtime [(DeviceId, WakeGrant)],
    stop_grants: &'runtime [(DeviceId, StopGrant)],
    access_ports: &'runtime RuntimeAccessPorts,
}

impl RuntimeDeviceContext<'_, '_> {
    fn has_grant<T>(&self, grants: &[(DeviceId, T)], matches_token: impl Fn(&T) -> bool) -> bool {
        grants.iter().any(|(device_id, registered)| {
            *device_id == self.device_id && matches_token(registered)
        })
    }
}

impl DeviceContext for RuntimeDeviceContext<'_, '_> {
    fn device_id(&self) -> DeviceId {
        self.device_id
    }

    fn read_guest_memory(
        &mut self,
        grant: &DmaGrant,
        addr: GuestPhysAddr,
        data: &mut [u8],
    ) -> Result<(), DeviceError> {
        if !self.has_grant(self.dma_grants, |registered| registered.same_token(grant)) {
            return Err(DeviceError::Unsupported {
                operation: "read guest memory from device access",
                detail: "device has no DMA memory grant".into(),
            });
        }
        let memory = self
            .memory
            .as_mut()
            .ok_or_else(|| DeviceError::Unsupported {
                operation: "read guest memory from device access",
                detail: "this bus access has no DMA memory port".into(),
            })?;
        memory.read(addr, data)
    }
    fn write_guest_memory(
        &mut self,
        grant: &DmaGrant,
        addr: GuestPhysAddr,
        data: &[u8],
    ) -> Result<(), DeviceError> {
        if !self.has_grant(self.dma_grants, |registered| registered.same_token(grant)) {
            return Err(DeviceError::Unsupported {
                operation: "write guest memory from device access",
                detail: "device has no DMA memory grant".into(),
            });
        }
        let memory = self
            .memory
            .as_mut()
            .ok_or_else(|| DeviceError::Unsupported {
                operation: "write guest memory from device access",
                detail: "this bus access has no DMA memory port".into(),
            })?;
        memory.write(addr, data)
    }

    fn schedule_timer(&mut self, grant: &TimerGrant, deadline_ns: u64) -> Result<(), DeviceError> {
        if !self.has_grant(self.timer_grants, |registered| registered.same_token(grant)) {
            return Err(DeviceError::Unsupported {
                operation: "schedule timer from device access",
                detail: "device has no timer grant".into(),
            });
        }
        let timer = self
            .access_ports
            .timer
            .as_ref()
            .ok_or_else(|| DeviceError::Unsupported {
                operation: "schedule timer from device access",
                detail: "no timer port is attached to this VM runtime".into(),
            })?;
        timer
            .schedule_timer(self.device_id, deadline_ns)
            .map_err(DeviceError::from)
    }

    fn wake_vcpu(&mut self, grant: &WakeGrant, vcpu_id: usize) -> Result<(), DeviceError> {
        if !self.has_grant(self.wake_grants, |registered| registered.same_token(grant)) {
            return Err(DeviceError::Unsupported {
                operation: "wake vCPU from device access",
                detail: "device has no wake grant".into(),
            });
        }
        let wake = self
            .access_ports
            .wake
            .as_ref()
            .ok_or_else(|| DeviceError::Unsupported {
                operation: "wake vCPU from device access",
                detail: "no vCPU wake port is attached to this VM runtime".into(),
            })?;
        wake.wake_vcpu(self.device_id, vcpu_id)
            .map_err(DeviceError::from)
    }

    fn request_vm_stop(&mut self, grant: &StopGrant, reason: &str) -> Result<(), DeviceError> {
        if !self.has_grant(self.stop_grants, |registered| registered.same_token(grant)) {
            return Err(DeviceError::Unsupported {
                operation: "request VM stop from device access",
                detail: "device has no stop grant".into(),
            });
        }
        let stop = self
            .access_ports
            .stop
            .as_ref()
            .ok_or_else(|| DeviceError::Unsupported {
                operation: "request VM stop from device access",
                detail: "no VM stop port is attached to this VM runtime".into(),
            })?;
        stop.request_vm_stop(self.device_id, reason)
            .map_err(DeviceError::from)
    }
}

impl DeviceRuntime {
    pub(crate) fn empty() -> Self {
        Self {
            devices: Vec::new(),
            mmio_index: BTreeMap::new(),
            port_index: BTreeMap::new(),
            sysreg_index: BTreeMap::new(),
            pollable_devices: Vec::new(),
            dma_pollable_devices: Vec::new(),
            lifecycle_devices: Vec::new(),
            services: DeviceServices::new(),
            planned: PlannedRuntimeResources::new(),
            dma_grants: Vec::new(),
            timer_grants: Vec::new(),
            wake_grants: Vec::new(),
            stop_grants: Vec::new(),
            access_ports: RuntimeAccessPorts::new(),
            sealed: false,
            pci_roots: BTreeMap::new(),
            pci_binding_leases: Vec::new(),
        }
    }

    pub(crate) fn attach_access_ports(&mut self, access_ports: RuntimeAccessPorts) {
        self.access_ports = access_ports;
    }

    pub(crate) const fn interrupt_registry(&self) -> &crate::interrupt::InterruptRegistry {
        &self.planned.interrupts
    }

    /// Freezes this runtime topology after VM preparation.
    pub(crate) fn seal(&mut self) {
        self.sealed = true;
    }

    fn ensure_unsealed(&self, operation: &'static str) -> DeviceManagerResult {
        if self.sealed {
            return Err(DeviceManagerError::InvalidState {
                operation,
                detail: "device runtime topology is sealed".into(),
            });
        }
        Ok(())
    }

    /// Returns a typed service contributed by one emulated device.
    ///
    /// # Errors
    ///
    /// Returns an error when no provider exists for `K`, or when `K` is a
    /// multi-provider service that must be queried through a future explicit
    /// multi-provider runtime API.
    pub fn service<K: ServiceKey>(&self) -> DeviceManagerResult<Arc<K::Service>> {
        self.services.require::<K>()
    }

    /// Registers a bundle atomically.  If any device fails to register,
    /// already-registered devices in this bundle are rolled back via
    /// `pop()` + index-key removal.
    pub fn register_bundle(&mut self, bundle: DeviceBundle) -> DeviceManagerResult {
        if bundle.pci_function.is_some() || !bundle.services.all::<PciRootBindingKey>().is_empty() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "register device bundle",
                detail: "PCI bindings require resolved graph-node registration".into(),
            });
        }
        self.register_bundle_inner(bundle)
    }

    fn register_bundle_inner(&mut self, bundle: DeviceBundle) -> DeviceManagerResult {
        self.ensure_unsealed("register device bundle")?;
        validate_bundle_grant_indices(
            bundle.devices.len(),
            &bundle.guest_memory_devices,
            "register device guest-memory capability",
            "guest-memory capability",
        )?;
        validate_bundle_grant_indices(
            bundle.devices.len(),
            &bundle
                .dma_pollable
                .iter()
                .map(|(index, _, grant)| (*index, grant.clone()))
                .collect::<Vec<_>>(),
            "register DMA-pollable device",
            "DMA-pollable capability",
        )?;
        validate_bundle_grant_indices(
            bundle.devices.len(),
            &bundle.timer_devices,
            "register device timer capability",
            "timer capability",
        )?;
        validate_bundle_grant_indices(
            bundle.devices.len(),
            &bundle.wake_devices,
            "register device wake capability",
            "wake capability",
        )?;
        validate_bundle_grant_indices(
            bundle.devices.len(),
            &bundle.stop_devices,
            "register device stop capability",
            "stop capability",
        )?;
        for (index, pollable) in bundle.pollable.iter().enumerate() {
            if self
                .pollable_devices
                .iter()
                .chain(bundle.pollable[..index].iter())
                .any(|existing| Arc::ptr_eq(existing, pollable))
            {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register pollable device",
                    detail: "the same pollable capability is already registered".into(),
                });
            }
        }
        for (index, (_, pollable, _)) in bundle.dma_pollable.iter().enumerate() {
            if self
                .dma_pollable_devices
                .iter()
                .map(|(_, existing, _)| existing)
                .chain(
                    bundle.dma_pollable[..index]
                        .iter()
                        .map(|(_, existing, _)| existing),
                )
                .any(|existing| Arc::ptr_eq(existing, pollable))
            {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register DMA-pollable device",
                    detail: "the same DMA-pollable capability is already registered".into(),
                });
            }
        }
        for (index, lifecycle) in bundle.lifecycle.iter().enumerate() {
            if self
                .lifecycle_devices
                .iter()
                .chain(bundle.lifecycle[..index].iter())
                .any(|existing| Arc::ptr_eq(existing, lifecycle))
            {
                return Err(DeviceManagerError::ResourceConflict {
                    operation: "register device lifecycle",
                    detail: "the same lifecycle capability is already registered".into(),
                });
            }
        }
        self.services.validate_merge(&bundle.services)?;
        self.planned.validate_bundle(&bundle.planned)?;

        let saved_len = self.devices.len();
        for device in &bundle.devices {
            if let Err(error) = self.register(device.clone()) {
                self.truncate_devices(saved_len);
                return Err(error.into());
            }
        }
        self.dma_grants.extend(
            bundle
                .guest_memory_devices
                .iter()
                .map(|(index, grant)| (DeviceId::new((saved_len + index) as u32), grant.clone())),
        );
        self.timer_grants.extend(
            bundle
                .timer_devices
                .iter()
                .map(|(index, grant)| (DeviceId::new((saved_len + index) as u32), grant.clone())),
        );
        self.wake_grants.extend(
            bundle
                .wake_devices
                .iter()
                .map(|(index, grant)| (DeviceId::new((saved_len + index) as u32), grant.clone())),
        );
        self.stop_grants.extend(
            bundle
                .stop_devices
                .iter()
                .map(|(index, grant)| (DeviceId::new((saved_len + index) as u32), grant.clone())),
        );
        self.pollable_devices.extend(bundle.pollable);
        self.dma_pollable_devices
            .extend(
                bundle
                    .dma_pollable
                    .into_iter()
                    .map(|(index, pollable, grant)| {
                        (DeviceId::new((saved_len + index) as u32), pollable, grant)
                    }),
            );
        self.lifecycle_devices.extend(bundle.lifecycle);
        self.services.append(bundle.services);
        self.planned.append(bundle.planned);
        Ok(())
    }

    /// Registers one graph node's bundle with PCI identity validation.
    ///
    /// Host nodes must publish exactly one [`PciRootBindingKey`] service whose
    /// owner and topology match the resolved node; endpoint bundles must bind
    /// their resolved function through a dependency-hosted root. The binding
    /// lease is retained by the runtime and its `Drop` unwinds the provisional
    /// route (invalidate generation -> withdraw root route -> release the
    /// endpoint reference), so any failure leaves no residue.
    pub(crate) fn register_graph_bundle(
        &mut self,
        node: &ResolvedDeviceNode,
        bundle: DeviceBundle,
    ) -> DeviceManagerResult {
        let root_services = bundle.services.all::<PciRootBindingKey>();
        let root = match node.pci_host_topology() {
            Some(topology) => {
                if root_services.len() != 1 {
                    return Err(DeviceManagerError::InvalidConfig {
                        operation: "register PCI host bundle",
                        detail: alloc::format!(
                            "host {} must publish exactly one PciRootBinding",
                            node.id()
                        ),
                    });
                }
                let root = root_services[0].clone();
                if root.host() != node.id()
                    || !root.matches_topology(topology)
                    || self.pci_roots.contains_key(node.id())
                {
                    return Err(DeviceManagerError::InvalidConfig {
                        operation: "register PCI host bundle",
                        detail: alloc::format!(
                            "host {} published a mismatched PCI root",
                            node.id()
                        ),
                    });
                }
                Some(root)
            }
            None if root_services.is_empty() => None,
            None => {
                return Err(DeviceManagerError::InvalidConfig {
                    operation: "register PCI host bundle",
                    detail: alloc::format!("non-host node {} published a PCI root", node.id()),
                });
            }
        };

        let endpoint = match (node.pci_endpoint(), bundle.pci_function.as_ref()) {
            (Some(endpoint), Some(function)) => {
                if function.device_index >= bundle.devices.len()
                    || !node.dependencies().contains(&endpoint.host)
                {
                    return Err(DeviceManagerError::InvalidConfig {
                        operation: "register PCI endpoint bundle",
                        detail: alloc::format!(
                            "endpoint {} has an invalid PCI device binding",
                            node.id()
                        ),
                    });
                }
                let binding = self.pci_roots.get(&endpoint.host).ok_or_else(|| {
                    DeviceManagerError::ResourceNotFound {
                        operation: "register PCI endpoint bundle",
                        resource: alloc::format!("PCI root dependency {}", endpoint.host),
                    }
                })?;
                let device = DeviceId::new((self.devices.len() + function.device_index) as u32);
                Some(binding.bind(&endpoint.function_node, device, function.function.clone())?)
            }
            (Some(_), None) => {
                return Err(DeviceManagerError::InvalidConfig {
                    operation: "register PCI endpoint bundle",
                    detail: alloc::format!(
                        "endpoint {} did not declare its bundled PCI function",
                        node.id()
                    ),
                });
            }
            (None, Some(_)) => {
                return Err(DeviceManagerError::InvalidConfig {
                    operation: "register PCI endpoint bundle",
                    detail: alloc::format!(
                        "non-PCI node {} declared a bundled PCI function",
                        node.id()
                    ),
                });
            }
            (None, None) => None,
        };

        self.register_bundle_inner(bundle)?;
        if let Some(root) = root {
            self.pci_roots.insert(node.id().clone(), root);
        }
        if let Some(endpoint) = endpoint {
            self.pci_binding_leases.push(endpoint);
        }
        Ok(())
    }

    fn truncate_devices(&mut self, len: usize) {
        while self.devices.len() > len {
            let device = self
                .devices
                .pop()
                .expect("device length was checked before rollback");
            self.remove_resources(device.resources());
        }
    }

    fn remove_resources(&mut self, resources: &[Resource]) {
        for resource in resources {
            match *resource {
                Resource::MmioRange { base, .. } => {
                    self.mmio_index.remove(&base);
                }
                Resource::PortRange { base, .. } => {
                    self.port_index.remove(&base);
                }
                Resource::SysReg { addr, .. } => {
                    self.sysreg_index.remove(&addr);
                }
                Resource::IrqLine { .. } => {}
            }
        }
    }

    /// Validates every resource without mutating the dispatch indices.
    fn validate_resources(&self, resources: &[Resource]) -> Result<(), RegistryError> {
        for (index, resource) in resources.iter().enumerate() {
            let earlier_resources = &resources[..index];
            match *resource {
                Resource::MmioRange { base, size } => {
                    self.validate_mmio_range(base, size, earlier_resources)?;
                }
                Resource::PortRange { base, size } => {
                    self.validate_port_range(base, size, earlier_resources)?;
                }
                Resource::SysReg { addr, count } => {
                    self.validate_sysreg_range(addr, count, earlier_resources)?;
                }
                Resource::IrqLine { line, trigger } => {
                    if earlier_resources.iter().any(
                        |resource| matches!(resource, Resource::IrqLine { line: earlier, .. } if *earlier == line),
                    ) {
                        return Err(RegistryError::InvalidResource {
                            resource: Resource::IrqLine { line, trigger },
                            reason: InvalidResourceReason::DuplicateIrqLine { line },
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_mmio_range(
        &self,
        base: u64,
        size: u64,
        earlier_resources: &[Resource],
    ) -> Result<(), RegistryError> {
        let resource = Resource::MmioRange { base, size };
        if size == 0 {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::ZeroSized,
            });
        }
        let Some(end) = base.checked_add(size) else {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::AddressOverflow,
            });
        };
        if earlier_resources.iter().any(|earlier| {
            matches!(
                *earlier,
                Resource::MmioRange {
                    base: earlier_base,
                    size: earlier_size,
                } if ranges_overlap(
                    base,
                    end,
                    earlier_base,
                    earlier_base.saturating_add(earlier_size),
                )
            )
        }) {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::OverlappingResources,
            });
        }
        if let Some((existing_base, existing)) = self.mmio_conflict(base, end) {
            return Err(RegistryError::AddressConflict {
                resource,
                existing: Resource::MmioRange {
                    base: existing_base,
                    size: existing.size,
                },
                existing_device: DeviceId::new(existing.slot as u32),
            });
        }
        Ok(())
    }

    fn validate_port_range(
        &self,
        base: u16,
        size: u16,
        earlier_resources: &[Resource],
    ) -> Result<(), RegistryError> {
        let resource = Resource::PortRange { base, size };
        if size == 0 {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::ZeroSized,
            });
        }
        let end = base as u64 + size as u64;
        if end > u16::MAX as u64 + 1 {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::AddressOverflow,
            });
        }
        if earlier_resources.iter().any(|earlier| {
            matches!(
                *earlier,
                Resource::PortRange {
                    base: earlier_base,
                    size: earlier_size,
                } if ranges_overlap(
                    base as u64,
                    end,
                    earlier_base as u64,
                    earlier_base as u64 + earlier_size as u64,
                )
            )
        }) {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::OverlappingResources,
            });
        }
        if let Some((existing_base, existing)) = self.port_conflict(base, end) {
            return Err(RegistryError::AddressConflict {
                resource,
                existing: Resource::PortRange {
                    base: existing_base,
                    size: existing.size as u16,
                },
                existing_device: DeviceId::new(existing.slot as u32),
            });
        }
        Ok(())
    }

    fn validate_sysreg_range(
        &self,
        addr: u32,
        count: u32,
        earlier_resources: &[Resource],
    ) -> Result<(), RegistryError> {
        let resource = Resource::SysReg { addr, count };
        if count == 0 {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::ZeroSized,
            });
        }
        let end = addr as u64 + count as u64;
        if end > u32::MAX as u64 + 1 {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::AddressOverflow,
            });
        }
        if earlier_resources.iter().any(|earlier| {
            matches!(
                *earlier,
                Resource::SysReg {
                    addr: earlier_addr,
                    count: earlier_count,
                } if ranges_overlap(
                    addr as u64,
                    end,
                    earlier_addr as u64,
                    earlier_addr as u64 + earlier_count as u64,
                )
            )
        }) {
            return Err(RegistryError::InvalidResource {
                resource,
                reason: InvalidResourceReason::OverlappingResources,
            });
        }
        if let Some((existing_addr, existing)) = self.sysreg_conflict(addr, end) {
            return Err(RegistryError::AddressConflict {
                resource,
                existing: Resource::SysReg {
                    addr: existing_addr,
                    count: existing.size as u32,
                },
                existing_device: DeviceId::new(existing.slot as u32),
            });
        }
        Ok(())
    }

    fn mmio_conflict(&self, base: u64, end: u64) -> Option<(u64, &RangeEntry)> {
        if let Some((&existing_base, existing)) = self.mmio_index.range(..=base).next_back()
            && base < existing_base.saturating_add(existing.size)
        {
            return Some((existing_base, existing));
        }
        self.mmio_index
            .range(base..)
            .next()
            .filter(|(existing_base, _)| **existing_base < end)
            .map(|(&existing_base, existing)| (existing_base, existing))
    }

    fn port_conflict(&self, base: u16, end: u64) -> Option<(u16, &RangeEntry)> {
        if let Some((&existing_base, existing)) = self.port_index.range(..=base).next_back()
            && (base as u64) < existing_base as u64 + existing.size
        {
            return Some((existing_base, existing));
        }
        self.port_index
            .range(base..)
            .next()
            .filter(|(existing_base, _)| (**existing_base as u64) < end)
            .map(|(&existing_base, existing)| (existing_base, existing))
    }

    fn sysreg_conflict(&self, addr: u32, end: u64) -> Option<(u32, &RangeEntry)> {
        if let Some((&existing_addr, existing)) = self.sysreg_index.range(..=addr).next_back()
            && (addr as u64) < existing_addr as u64 + existing.size
        {
            return Some((existing_addr, existing));
        }
        self.sysreg_index
            .range(addr..)
            .next()
            .filter(|(existing_addr, _)| (**existing_addr as u64) < end)
            .map(|(&existing_addr, existing)| (existing_addr, existing))
    }

    fn insert_resources(&mut self, idx: usize, resources: &[Resource]) {
        for resource in resources {
            match *resource {
                Resource::MmioRange { base, size } => {
                    self.mmio_index.insert(base, RangeEntry { slot: idx, size });
                }
                Resource::PortRange { base, size } => {
                    self.port_index.insert(
                        base,
                        RangeEntry {
                            slot: idx,
                            size: size as u64,
                        },
                    );
                }
                Resource::SysReg { addr, count } => {
                    self.sysreg_index.insert(
                        addr,
                        RangeEntry {
                            slot: idx,
                            size: count as u64,
                        },
                    );
                }
                Resource::IrqLine { .. } => {}
            }
        }
    }

    // ─── Lookup helpers ────────────────────────────────────────────

    fn lookup_mmio(&self, addr: u64, width: AccessWidth) -> Option<usize> {
        let (&base, entry) = self.mmio_index.range(..=addr).next_back()?;
        range_contains_access(base, entry.size, addr, width).then_some(entry.slot)
    }

    fn lookup_port(&self, addr: u16, width: AccessWidth) -> Option<usize> {
        let (&base, entry) = self.port_index.range(..=addr).next_back()?;
        range_contains_access(base as u64, entry.size, addr as u64, width).then_some(entry.slot)
    }

    fn lookup_sysreg(&self, addr: u32) -> Option<usize> {
        let (&start, entry) = self.sysreg_index.range(..=addr).next_back()?;
        let end = start.saturating_add((entry.size as u32).saturating_sub(1));
        (addr <= end).then_some(entry.slot)
    }

    // ─── Public helpers ───────────────────────────────────────────

    /// Returns an iterator over all currently registered devices.
    pub fn devices(&self) -> impl Iterator<Item = &dyn Device> {
        self.devices.iter().map(|slot| &**slot)
    }

    /// Returns the number of currently registered devices.
    pub fn device_count(&self) -> usize {
        self.devices.len()
    }

    // ─── Iterator helpers ───────────────────────────────────────────
    //
    // NOTE: With the unified Device trait, [`devices()`] is the canonical
    // iterator. Use [`Device::resources()`] or typed service registries for
    // per-bus filtering in new code.

    /// Iterates over devices that require periodic polling.
    pub fn iter_pollable_dev(&self) -> impl Iterator<Item = &Arc<dyn PollableDeviceOps>> {
        self.pollable_devices.iter()
    }

    /// Polls asynchronous DMA devices with a guest-memory port scoped to each
    /// individual callback.
    pub fn poll_dma_devices(
        &self,
        now_ns: u64,
        memory: &mut dyn GuestMemoryAccess,
        mut observe: impl FnMut(DeviceManagerResult),
    ) {
        for (device_id, pollable, grant) in &self.dma_pollable_devices {
            let mut context = RuntimeDeviceContext {
                device_id: *device_id,
                memory: Some(&mut *memory),
                dma_grants: &self.dma_grants,
                timer_grants: &self.timer_grants,
                wake_grants: &self.wake_grants,
                stop_grants: &self.stop_grants,
                access_ports: &self.access_ports,
            };
            observe(pollable.poll_dma(now_ns, &mut context, grant));
        }
    }

    /// Returns VM-local typed device services.
    pub const fn services(&self) -> &DeviceServices {
        &self.services
    }

    /// Returns a registered wired interrupt-controller capability.
    pub fn interrupt_controller(
        &self,
        id: axdevice_base::InterruptControllerId,
    ) -> DeviceManagerResult<Arc<dyn axdevice_base::VirtualInterruptController>> {
        self.planned.interrupts.wired_controller(id)
    }

    /// Returns a registered message interrupt-controller capability.
    pub fn message_interrupt_controller(
        &self,
        id: axdevice_base::InterruptControllerId,
    ) -> DeviceManagerResult<Arc<dyn axdevice_base::MessageInterruptController>> {
        self.planned.interrupts.message_controller(id)
    }

    /// Resets lifecycle-capable devices in registration order.
    pub fn reset_lifecycle_devices(&self) -> DeviceManagerResult {
        for lifecycle in &self.lifecycle_devices {
            lifecycle.reset()?;
        }
        Ok(())
    }

    /// Suspends lifecycle-capable devices in reverse registration order.
    pub fn suspend_lifecycle_devices(&self) -> DeviceManagerResult {
        for lifecycle in self.lifecycle_devices.iter().rev() {
            lifecycle.suspend()?;
        }
        Ok(())
    }

    /// Resumes lifecycle-capable devices in registration order.
    pub fn resume_lifecycle_devices(&self) -> DeviceManagerResult {
        for lifecycle in &self.lifecycle_devices {
            lifecycle.resume()?;
        }
        Ok(())
    }

    // ─── Hot-path dispatch ──────────────────────────────────────────

    /// Reads from the device selected by `access`.
    ///
    /// A missing mapping is returned as `None`, allowing architecture fault
    /// handlers to continue with their non-device policy without a second
    /// interval lookup.
    pub fn try_read(&self, access: &DeviceAccess) -> DeviceManagerResult<Option<u64>> {
        let Some(index) = self
            .lookup_access(access)
            .map_err(|source| access_error("read", access, source))?
        else {
            return Ok(None);
        };
        let mut context = self.context_for(index, None);
        self.devices[index]
            .read(access, &mut context)
            .map(Some)
            .map_err(|source| access_error("read", access, source))
    }

    /// Writes to the device selected by `access`.
    ///
    /// `memory` is an optional VM-owned guest-memory port. Devices still need
    /// their matching [`DmaGrant`] before the context delegates to this port.
    pub fn try_write(
        &self,
        access: &DeviceAccess,
        value: u64,
        memory: Option<&mut dyn GuestMemoryAccess>,
    ) -> DeviceManagerResult<bool> {
        let Some(index) = self
            .lookup_access(access)
            .map_err(|source| access_error("write", access, source))?
        else {
            return Ok(false);
        };
        let mut context = self.context_for(index, memory);
        self.devices[index]
            .write(access, value, &mut context)
            .map(|()| true)
            .map_err(|source| access_error("write", access, source))
    }

    fn lookup_access(&self, access: &DeviceAccess) -> DeviceResult<Option<usize>> {
        match access.bus() {
            BusKind::Mmio => Ok(self.lookup_mmio(access.address(), access.width())),
            BusKind::Port => {
                let port =
                    u16::try_from(access.address()).map_err(|_| DeviceError::OutOfRange {
                        addr: access.address(),
                    })?;
                Ok(self.lookup_port(port, access.width()))
            }
            BusKind::SysReg => {
                let register =
                    u32::try_from(access.address()).map_err(|_| DeviceError::OutOfRange {
                        addr: access.address(),
                    })?;
                Ok(self.lookup_sysreg(register))
            }
        }
    }

    fn context_for<'runtime, 'memory>(
        &'runtime self,
        index: usize,
        memory: Option<&'memory mut dyn GuestMemoryAccess>,
    ) -> RuntimeDeviceContext<'runtime, 'memory> {
        RuntimeDeviceContext {
            device_id: DeviceId::new(index as u32),
            memory,
            dma_grants: &self.dma_grants,
            timer_grants: &self.timer_grants,
            wake_grants: &self.wake_grants,
            stop_grants: &self.stop_grants,
            access_ports: &self.access_ports,
        }
    }
}

fn access_error(
    operation: &'static str,
    access: &DeviceAccess,
    source: DeviceError,
) -> DeviceManagerError {
    DeviceManagerError::Access {
        operation,
        bus: access.bus(),
        addr: access.address(),
        width: access.width(),
        source,
    }
}

impl Default for DeviceRuntime {
    fn default() -> Self {
        Self::empty()
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl DeviceRegistry for DeviceRuntime {
    fn register(&mut self, device: Arc<dyn Device>) -> Result<DeviceId, RegistryError> {
        if self.sealed {
            return Err(RegistryError::InvalidState {
                operation: "register device",
                detail: "device runtime topology is sealed".into(),
            });
        }
        let idx = self.devices.len();
        self.validate_resources(device.resources())?;
        self.insert_resources(idx, device.resources());
        self.devices.push(device);
        info!("DeviceRuntime: registered device id={}", idx);
        Ok(DeviceId::new(idx as u32))
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use axdevice_base::{
        AccessWidth, BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DeviceId,
        DeviceRegistry, DeviceVcpuId, DmaGrant, GuestMemoryAccess, InvalidResourceReason,
        RegistryError, Resource, StopGrant, TimerGrant, WakeGrant,
    };
    use axvm_types::GuestPhysAddr;

    use super::{
        DeviceRuntime, RuntimeAccessPorts, StopAccessPort, TimerAccessPort, WakeAccessPort,
    };
    use crate::{
        DeviceBundle, DeviceLifecycle, DeviceManagerError, DeviceManagerResult, DeviceRegistration,
        DmaPollableDeviceOps, ServiceCardinality, ServiceKey,
    };

    struct D {
        resources: alloc::vec::Vec<Resource>,
        n: &'static str,
    }
    impl D {
        fn new_mmio(a: u64, s: u64, n: &'static str) -> Self {
            Self {
                resources: alloc::vec![Resource::MmioRange { base: a, size: s }],
                n,
            }
        }
        fn new_port(base: u16, size: u16, n: &'static str) -> Self {
            Self {
                resources: alloc::vec![Resource::PortRange { base, size }],
                n,
            }
        }
    }

    struct AccessAwareDevice {
        resources: alloc::vec::Vec<Resource>,
        expected_vcpu: DeviceVcpuId,
    }

    struct GuestMemoryRequestDevice {
        resources: alloc::vec::Vec<Resource>,
        dma_grant: DmaGrant,
    }

    enum SensitiveGrantKind {
        Timer,
        Wake,
        Stop,
    }

    struct SensitiveGrantRequestDevice {
        resources: alloc::vec::Vec<Resource>,
        kind: SensitiveGrantKind,
        timer_grant: TimerGrant,
        wake_grant: WakeGrant,
        stop_grant: StopGrant,
    }

    struct CountingTimerPort(Arc<AtomicUsize>);

    impl TimerAccessPort for CountingTimerPort {
        fn schedule_timer(&self, _device_id: DeviceId, deadline_ns: u64) -> DeviceManagerResult {
            assert_eq!(deadline_ns, 42);
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct CountingWakePort(Arc<AtomicUsize>);

    impl WakeAccessPort for CountingWakePort {
        fn wake_vcpu(&self, _device_id: DeviceId, vcpu_id: usize) -> DeviceManagerResult {
            assert_eq!(vcpu_id, 0);
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct CountingStopPort(Arc<AtomicUsize>);

    impl StopAccessPort for CountingStopPort {
        fn request_vm_stop(&self, _device_id: DeviceId, reason: &str) -> DeviceManagerResult {
            assert_eq!(reason, "test stop request");
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[cfg(target_arch = "x86_64")]
    struct ServiceBackedIoApic {
        resources: alloc::vec::Vec<Resource>,
    }

    #[cfg(target_arch = "x86_64")]
    impl Device for ServiceBackedIoApic {
        fn name(&self) -> &str {
            "service-backed-ioapic"
        }

        fn resources(&self) -> &[Resource] {
            &self.resources
        }

        fn read(
            &self,
            _access: &DeviceAccess,
            _context: &mut dyn DeviceContext,
        ) -> Result<u64, DeviceError> {
            Ok(0)
        }

        fn write(
            &self,
            _access: &DeviceAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> Result<(), DeviceError> {
            Ok(())
        }
    }

    #[cfg(target_arch = "x86_64")]
    impl crate::X86IoApicDeviceOps for ServiceBackedIoApic {
        fn vector_for_gsi(&self, gsi: usize) -> Option<u8> {
            (gsi == 4).then_some(0x44)
        }

        fn assert_gsi(&self, _gsi: usize) -> Option<x86_vlapic::IoApicInterrupt> {
            None
        }

        fn set_gsi_level(
            &self,
            _gsi: usize,
            _asserted: bool,
        ) -> Option<x86_vlapic::IoApicInterrupt> {
            None
        }

        fn end_of_interrupt(&self, _vector: u8) -> Option<x86_vlapic::IoApicEoi> {
            None
        }
    }

    impl Device for AccessAwareDevice {
        fn name(&self) -> &str {
            "access-aware"
        }

        fn resources(&self) -> &[Resource] {
            &self.resources
        }

        fn read(
            &self,
            access: &DeviceAccess,
            context: &mut dyn DeviceContext,
        ) -> Result<u64, DeviceError> {
            assert_eq!(context.device_id(), DeviceId::new(0));
            assert_eq!(access.source_vcpu(), self.expected_vcpu);
            Ok(0xfeed)
        }

        fn write(
            &self,
            access: &DeviceAccess,
            _value: u64,
            context: &mut dyn DeviceContext,
        ) -> Result<(), DeviceError> {
            assert_eq!(context.device_id(), DeviceId::new(0));
            assert_eq!(access.source_vcpu(), self.expected_vcpu);
            Ok(())
        }
    }

    impl Device for GuestMemoryRequestDevice {
        fn name(&self) -> &str {
            "guest-memory-request"
        }

        fn resources(&self) -> &[Resource] {
            &self.resources
        }

        fn read(
            &self,
            _access: &DeviceAccess,
            _context: &mut dyn DeviceContext,
        ) -> Result<u64, DeviceError> {
            Err(DeviceError::WriteOnly)
        }

        fn write(
            &self,
            _access: &DeviceAccess,
            _value: u64,
            context: &mut dyn DeviceContext,
        ) -> Result<(), DeviceError> {
            let mut byte = [0u8; 1];
            context.read_guest_memory(&self.dma_grant, GuestPhysAddr::from_usize(0), &mut byte)?;
            Ok(())
        }
    }

    impl Device for SensitiveGrantRequestDevice {
        fn name(&self) -> &str {
            "sensitive-grant-request"
        }

        fn resources(&self) -> &[Resource] {
            &self.resources
        }

        fn read(
            &self,
            _access: &DeviceAccess,
            _context: &mut dyn DeviceContext,
        ) -> Result<u64, DeviceError> {
            Err(DeviceError::WriteOnly)
        }

        fn write(
            &self,
            _access: &DeviceAccess,
            _value: u64,
            context: &mut dyn DeviceContext,
        ) -> Result<(), DeviceError> {
            match self.kind {
                SensitiveGrantKind::Timer => context.schedule_timer(&self.timer_grant, 42)?,
                SensitiveGrantKind::Wake => context.wake_vcpu(&self.wake_grant, 0)?,
                SensitiveGrantKind::Stop => {
                    context.request_vm_stop(&self.stop_grant, "test stop request")?
                }
            }
            Ok(())
        }
    }

    struct TestMemoryPort;

    impl GuestMemoryAccess for TestMemoryPort {
        fn read(&mut self, _addr: GuestPhysAddr, _data: &mut [u8]) -> Result<(), DeviceError> {
            Ok(())
        }

        fn write(&mut self, _addr: GuestPhysAddr, _data: &[u8]) -> Result<(), DeviceError> {
            Ok(())
        }
    }

    struct TestDmaPoller {
        grant: DmaGrant,
        polls: Arc<AtomicUsize>,
    }

    impl DmaPollableDeviceOps for TestDmaPoller {
        fn poll_dma(
            &self,
            _now_ns: u64,
            context: &mut dyn DeviceContext,
            _registered_grant: &DmaGrant,
        ) -> DeviceManagerResult {
            let mut byte = [0u8; 1];
            context
                .read_guest_memory(&self.grant, GuestPhysAddr::from_usize(0), &mut byte)
                .map_err(DeviceManagerError::Device)?;
            self.polls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    impl Device for D {
        fn name(&self) -> &str {
            self.n
        }
        fn resources(&self) -> &[Resource] {
            &self.resources
        }
        fn read(
            &self,
            _access: &DeviceAccess,
            _context: &mut dyn DeviceContext,
        ) -> Result<u64, DeviceError> {
            Ok(0)
        }

        fn write(
            &self,
            _access: &DeviceAccess,
            _value: u64,
            _context: &mut dyn DeviceContext,
        ) -> Result<(), DeviceError> {
            Ok(())
        }
    }

    trait BundleService: Send + Sync {
        fn value(&self) -> usize;
    }

    struct BundleServiceKey;

    impl ServiceKey for BundleServiceKey {
        type Service = dyn BundleService;

        const NAME: &'static str = "bundle-service";
        const CARDINALITY: ServiceCardinality = ServiceCardinality::Single;
    }

    struct BundleServiceProvider(usize);

    impl BundleService for BundleServiceProvider {
        fn value(&self) -> usize {
            self.0
        }
    }

    struct CountingLifecycle {
        reset_calls: AtomicUsize,
        suspend_calls: AtomicUsize,
        resume_calls: AtomicUsize,
    }

    impl DeviceLifecycle for CountingLifecycle {
        fn reset(&self) -> DeviceManagerResult {
            self.reset_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn suspend(&self) -> DeviceManagerResult {
            self.suspend_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }

        fn resume(&self) -> DeviceManagerResult {
            self.resume_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn vcpu_mmio_dispatch_propagates_the_explicit_accessor() {
        let mut devices = DeviceRuntime::empty();
        let vcpu_id = DeviceVcpuId::new(7);
        devices
            .register(Arc::new(AccessAwareDevice {
                resources: alloc::vec![Resource::MmioRange {
                    base: 0x4000,
                    size: 0x100,
                }],
                expected_vcpu: vcpu_id,
            }))
            .unwrap();

        assert_eq!(
            devices
                .try_read(&DeviceAccess::new(
                    vcpu_id,
                    BusKind::Mmio,
                    0x4000,
                    AccessWidth::Dword,
                ))
                .unwrap(),
            Some(0xfeed_u64)
        );
    }

    #[test]
    fn vcpu_port_dispatch_propagates_the_explicit_accessor() {
        let mut devices = DeviceRuntime::empty();
        let vcpu_id = DeviceVcpuId::new(7);
        devices
            .register(Arc::new(AccessAwareDevice {
                resources: alloc::vec![Resource::PortRange {
                    base: 0x4000,
                    size: 0x100,
                }],
                expected_vcpu: vcpu_id,
            }))
            .unwrap();

        assert_eq!(
            devices
                .try_read(&DeviceAccess::new(
                    vcpu_id,
                    BusKind::Port,
                    0x4000,
                    AccessWidth::Dword,
                ))
                .unwrap(),
            Some(0xfeed_u64)
        );
    }

    #[test]
    fn vcpu_sysreg_dispatch_propagates_the_explicit_accessor() {
        let mut devices = DeviceRuntime::empty();
        let vcpu_id = DeviceVcpuId::new(7);
        devices
            .register(Arc::new(AccessAwareDevice {
                resources: alloc::vec![Resource::SysReg {
                    addr: 0x4000,
                    count: 1,
                }],
                expected_vcpu: vcpu_id,
            }))
            .unwrap();

        assert_eq!(
            devices
                .try_read(&DeviceAccess::new(
                    vcpu_id,
                    BusKind::SysReg,
                    0x4000,
                    AccessWidth::Qword,
                ))
                .unwrap(),
            Some(0xfeed_u64)
        );
    }

    #[test]
    fn memory_port_is_denied_to_devices_without_dma_grant() {
        let mut devices = DeviceRuntime::empty();
        devices
            .register(Arc::new(GuestMemoryRequestDevice {
                resources: alloc::vec![Resource::MmioRange {
                    base: 0x5000,
                    size: 0x100,
                }],
                dma_grant: DmaGrant::new(),
            }))
            .unwrap();
        let mut memory = TestMemoryPort;

        let error = devices
            .try_write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Mmio,
                    0x5000,
                    AccessWidth::Dword,
                ),
                0,
                Some(&mut memory),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            DeviceManagerError::Access {
                source: DeviceError::Unsupported { .. },
                ..
            }
        ));
    }

    #[test]
    fn bundle_declared_memory_device_receives_memory_port() {
        let mut devices = DeviceRuntime::empty();
        let dma_grant = DmaGrant::new();
        devices
            .register_bundle(DeviceBundle::new().with_guest_memory_device_grant(
                Arc::new(GuestMemoryRequestDevice {
                    resources: alloc::vec![Resource::MmioRange {
                        base: 0x6000,
                        size: 0x100,
                    }],
                    dma_grant: dma_grant.clone(),
                }),
                dma_grant,
            ))
            .unwrap();
        let mut memory = TestMemoryPort;

        assert!(
            devices
                .try_write(
                    &DeviceAccess::new(
                        DeviceVcpuId::new(0),
                        BusKind::Mmio,
                        0x6000,
                        AccessWidth::Dword,
                    ),
                    0,
                    Some(&mut memory),
                )
                .unwrap()
        );
    }

    #[test]
    fn dma_polling_scopes_memory_to_the_registered_device_and_grant() {
        let mut devices = DeviceRuntime::empty();
        let grant = DmaGrant::new();
        let polls = Arc::new(AtomicUsize::new(0));
        devices
            .register_bundle({
                let mut bundle = DeviceBundle::new();
                bundle.add_dma_pollable_device(
                    Arc::new(D::new_mmio(0x7000, 0x100, "dma-poll-device")),
                    Arc::new(TestDmaPoller {
                        grant: grant.clone(),
                        polls: polls.clone(),
                    }),
                    grant,
                );
                bundle
            })
            .unwrap();
        let mut memory = TestMemoryPort;
        let mut result = None;

        devices.poll_dma_devices(123, &mut memory, |poll_result| result = Some(poll_result));

        assert!(result.unwrap().is_ok());
        assert_eq!(polls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn dma_polling_rejects_a_different_grant_token() {
        let mut devices = DeviceRuntime::empty();
        let registered_grant = DmaGrant::new();
        let polls = Arc::new(AtomicUsize::new(0));
        devices
            .register_bundle({
                let mut bundle = DeviceBundle::new();
                bundle.add_dma_pollable_device(
                    Arc::new(D::new_mmio(0x7100, 0x100, "wrong-dma-grant")),
                    Arc::new(TestDmaPoller {
                        grant: DmaGrant::new(),
                        polls: polls.clone(),
                    }),
                    registered_grant,
                );
                bundle
            })
            .unwrap();
        let mut memory = TestMemoryPort;
        let mut result = None;

        devices.poll_dma_devices(123, &mut memory, |poll_result| result = Some(poll_result));

        assert!(result.unwrap().is_err());
        assert_eq!(polls.load(Ordering::Relaxed), 0);
    }

    fn dispatch_sensitive_grant_probe(
        devices: &DeviceRuntime,
        base: u64,
    ) -> Result<(), DeviceError> {
        devices
            .try_write(
                &DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Mmio,
                    base,
                    AccessWidth::Dword,
                ),
                0,
                None,
            )
            .map(|_| ())
            .map_err(Into::into)
    }

    #[test]
    fn timer_wake_and_stop_grants_are_checked_by_device_id_and_token() {
        let timer_grant = TimerGrant::new();
        let wake_grant = WakeGrant::new();
        let stop_grant = StopGrant::new();

        let mut denied = DeviceRuntime::empty();
        denied
            .register(Arc::new(SensitiveGrantRequestDevice {
                resources: alloc::vec![Resource::MmioRange {
                    base: 0x8000,
                    size: 0x100,
                }],
                kind: SensitiveGrantKind::Timer,
                timer_grant: timer_grant.clone(),
                wake_grant: WakeGrant::new(),
                stop_grant: StopGrant::new(),
            }))
            .unwrap();
        let error = dispatch_sensitive_grant_probe(&denied, 0x8000).unwrap_err();
        assert!(matches!(
            error,
            DeviceError::Unsupported { detail, .. } if detail.contains("no timer grant")
        ));

        let mut granted = DeviceRuntime::empty();
        let mut bundle = DeviceBundle::new();
        let timer_index = bundle.add_device(Arc::new(SensitiveGrantRequestDevice {
            resources: alloc::vec![Resource::MmioRange {
                base: 0x8100,
                size: 0x100,
            }],
            kind: SensitiveGrantKind::Timer,
            timer_grant: timer_grant.clone(),
            wake_grant: WakeGrant::new(),
            stop_grant: StopGrant::new(),
        }));
        bundle.grant_timer_to_device(timer_index, timer_grant);
        let wake_index = bundle.add_device(Arc::new(SensitiveGrantRequestDevice {
            resources: alloc::vec![Resource::MmioRange {
                base: 0x8200,
                size: 0x100,
            }],
            kind: SensitiveGrantKind::Wake,
            timer_grant: TimerGrant::new(),
            wake_grant: wake_grant.clone(),
            stop_grant: StopGrant::new(),
        }));
        bundle.grant_wake_to_device(wake_index, wake_grant);
        let stop_index = bundle.add_device(Arc::new(SensitiveGrantRequestDevice {
            resources: alloc::vec![Resource::MmioRange {
                base: 0x8300,
                size: 0x100,
            }],
            kind: SensitiveGrantKind::Stop,
            timer_grant: TimerGrant::new(),
            wake_grant: WakeGrant::new(),
            stop_grant: stop_grant.clone(),
        }));
        bundle.grant_stop_to_device(stop_index, stop_grant);
        granted.register_bundle(bundle).unwrap();

        let timer_error = dispatch_sensitive_grant_probe(&granted, 0x8100).unwrap_err();
        assert!(matches!(
            timer_error,
            DeviceError::Unsupported { detail, .. } if detail.contains("no timer port")
        ));
        let wake_error = dispatch_sensitive_grant_probe(&granted, 0x8200).unwrap_err();
        assert!(matches!(
            wake_error,
            DeviceError::Unsupported { detail, .. } if detail.contains("no vCPU wake port")
        ));
        let stop_error = dispatch_sensitive_grant_probe(&granted, 0x8300).unwrap_err();
        assert!(matches!(
            stop_error,
            DeviceError::Unsupported { detail, .. } if detail.contains("no VM stop port")
        ));
    }

    #[test]
    fn timer_wake_and_stop_grants_call_attached_runtime_ports() {
        let timer_calls = Arc::new(AtomicUsize::new(0));
        let wake_calls = Arc::new(AtomicUsize::new(0));
        let stop_calls = Arc::new(AtomicUsize::new(0));
        let timer_grant = TimerGrant::new();
        let wake_grant = WakeGrant::new();
        let stop_grant = StopGrant::new();

        let mut devices = DeviceRuntime::empty();
        devices.access_ports = RuntimeAccessPorts::new()
            .with_timer(Arc::new(CountingTimerPort(timer_calls.clone())))
            .with_wake(Arc::new(CountingWakePort(wake_calls.clone())))
            .with_stop(Arc::new(CountingStopPort(stop_calls.clone())));

        let mut bundle = DeviceBundle::new();
        let timer_index = bundle.add_device(Arc::new(SensitiveGrantRequestDevice {
            resources: alloc::vec![Resource::MmioRange {
                base: 0x8400,
                size: 0x100,
            }],
            kind: SensitiveGrantKind::Timer,
            timer_grant: timer_grant.clone(),
            wake_grant: WakeGrant::new(),
            stop_grant: StopGrant::new(),
        }));
        bundle.grant_timer_to_device(timer_index, timer_grant);
        let wake_index = bundle.add_device(Arc::new(SensitiveGrantRequestDevice {
            resources: alloc::vec![Resource::MmioRange {
                base: 0x8500,
                size: 0x100,
            }],
            kind: SensitiveGrantKind::Wake,
            timer_grant: TimerGrant::new(),
            wake_grant: wake_grant.clone(),
            stop_grant: StopGrant::new(),
        }));
        bundle.grant_wake_to_device(wake_index, wake_grant);
        let stop_index = bundle.add_device(Arc::new(SensitiveGrantRequestDevice {
            resources: alloc::vec![Resource::MmioRange {
                base: 0x8600,
                size: 0x100,
            }],
            kind: SensitiveGrantKind::Stop,
            timer_grant: TimerGrant::new(),
            wake_grant: WakeGrant::new(),
            stop_grant: stop_grant.clone(),
        }));
        bundle.grant_stop_to_device(stop_index, stop_grant);
        devices.register_bundle(bundle).unwrap();

        dispatch_sensitive_grant_probe(&devices, 0x8400).unwrap();
        dispatch_sensitive_grant_probe(&devices, 0x8500).unwrap();
        dispatch_sensitive_grant_probe(&devices, 0x8600).unwrap();

        assert_eq!(timer_calls.load(Ordering::Relaxed), 1);
        assert_eq!(wake_calls.load(Ordering::Relaxed), 1);
        assert_eq!(stop_calls.load(Ordering::Relaxed), 1);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn x86_ioapic_registration_publishes_typed_service() {
        let mut devices = DeviceRuntime::empty();
        let ioapic = Arc::new(ServiceBackedIoApic {
            resources: alloc::vec![Resource::MmioRange {
                base: 0xfec0_0000,
                size: 0x1000,
            }],
        });
        let service: Arc<dyn crate::X86IoApicDeviceOps> = ioapic.clone();
        let bundle = DeviceBundle::from_registration(DeviceRegistration::Device(ioapic))
            .with_service::<crate::X86IoApicServiceKey>(service)
            .unwrap();
        devices.register_bundle(bundle).unwrap();

        assert_eq!(devices.device_count(), 1);
        assert_eq!(
            devices
                .services()
                .require::<crate::X86IoApicServiceKey>()
                .unwrap()
                .vector_for_gsi(4),
            Some(0x44)
        );
    }

    #[test]
    fn resource_validation_rejects_same_bus_overlap_but_allows_distinct_buses() {
        for resources in [
            alloc::vec![
                Resource::MmioRange {
                    base: 0x1000,
                    size: 0x200,
                },
                Resource::MmioRange {
                    base: 0x1100,
                    size: 0x200,
                },
            ],
            alloc::vec![
                Resource::MmioRange {
                    base: 0x1000,
                    size: 0x1000,
                },
                Resource::MmioRange {
                    base: 0x1800,
                    size: 0x100,
                },
            ],
        ] {
            let mut runtime = DeviceRuntime::empty();
            assert!(matches!(
                runtime.register(Arc::new(D {
                    resources,
                    n: "overlapping",
                })),
                Err(RegistryError::InvalidResource {
                    reason: InvalidResourceReason::OverlappingResources,
                    ..
                })
            ));
        }

        let mut runtime = DeviceRuntime::empty();
        runtime
            .register(Arc::new(D {
                resources: alloc::vec![
                    Resource::MmioRange {
                        base: 0x1000,
                        size: 0x100,
                    },
                    Resource::PortRange {
                        base: 0x1000,
                        size: 0x10,
                    },
                ],
                n: "dual-bus",
            }))
            .unwrap();
    }

    #[test]
    fn test_zero_size_returns_invalid_resource() {
        let mut m = DeviceRuntime::empty();
        let result = m.register(Arc::new(D::new_mmio(0x1000, 0, "zero")));
        assert!(matches!(
            result,
            Err(RegistryError::InvalidResource {
                reason: InvalidResourceReason::ZeroSized,
                ..
            })
        ));
    }

    #[test]
    fn test_mmio_overflow_returns_invalid_resource() {
        struct OverflowDevice;
        impl Device for OverflowDevice {
            fn name(&self) -> &str {
                "overflow"
            }
            fn resources(&self) -> &[Resource] {
                static R: [Resource; 1] = [Resource::MmioRange {
                    base: u64::MAX - 1,
                    size: 4,
                }];
                &R
            }
            fn read(
                &self,
                _: &DeviceAccess,
                _context: &mut dyn DeviceContext,
            ) -> Result<u64, DeviceError> {
                Err(DeviceError::NotFound)
            }

            fn write(
                &self,
                _: &DeviceAccess,
                _value: u64,
                _context: &mut dyn DeviceContext,
            ) -> Result<(), DeviceError> {
                Err(DeviceError::NotFound)
            }
        }

        let mut m = DeviceRuntime::empty();
        let result = m.register(Arc::new(OverflowDevice));
        assert!(matches!(
            result,
            Err(RegistryError::InvalidResource {
                reason: InvalidResourceReason::AddressOverflow,
                ..
            })
        ));
    }

    #[test]
    fn rejects_access_that_crosses_mmio_resource_boundary() {
        let mut m = DeviceRuntime::empty();
        m.register(Arc::new(D::new_mmio(0x1000, 0x8, "small")))
            .unwrap();
        let vcpu = DeviceVcpuId::new(0);
        assert!(
            !m.try_write(
                &DeviceAccess::new(vcpu, BusKind::Mmio, 0x1004, AccessWidth::Qword),
                0,
                None,
            )
            .unwrap()
        );
        // 0x1008 == base + size — NotFound.
        assert_eq!(
            m.try_read(&DeviceAccess::new(
                vcpu,
                BusKind::Mmio,
                0x1008,
                AccessWidth::Dword,
            ))
            .unwrap(),
            None
        );
    }

    #[test]
    fn rejects_port_access_that_crosses_resource_boundary() {
        let mut m = DeviceRuntime::empty();
        m.register(Arc::new(D::new_port(0x80, 2, "small-port")))
            .unwrap();

        assert_eq!(
            m.try_read(&DeviceAccess::new(
                DeviceVcpuId::new(0),
                BusKind::Port,
                0x81,
                AccessWidth::Word,
            ))
            .unwrap(),
            None
        );
    }

    #[test]
    fn register_bundle_rolls_back_devices_after_resource_conflict() {
        let mut devices = DeviceRuntime::empty();
        devices
            .register(Arc::new(D::new_mmio(0x1000, 0x100, "existing")))
            .unwrap();

        let bundle = DeviceBundle::from_registration(DeviceRegistration::Device(Arc::new(
            D::new_mmio(0x2000, 0x100, "first-bundle-device"),
        )))
        .with_registration(DeviceRegistration::Device(Arc::new(D::new_mmio(
            0x1080,
            0x100,
            "conflicting-bundle-device",
        ))));

        assert!(matches!(
            devices.register_bundle(bundle),
            Err(crate::DeviceManagerError::Registry(
                RegistryError::AddressConflict { .. }
            ))
        ));
        assert_eq!(devices.device_count(), 1);
        assert_eq!(
            devices
                .try_read(&DeviceAccess::new(
                    DeviceVcpuId::new(0),
                    BusKind::Mmio,
                    0x2000,
                    AccessWidth::Dword,
                ))
                .unwrap(),
            None
        );
    }

    #[test]
    fn register_bundle_rejects_conflicting_service_without_registering_device() {
        let mut devices = DeviceRuntime::empty();
        let first_provider: Arc<dyn BundleService> = Arc::new(BundleServiceProvider(1));
        let mut first = DeviceBundle::new();
        first
            .provide_service::<BundleServiceKey>(first_provider)
            .unwrap();
        devices.register_bundle(first).unwrap();

        let conflicting_provider: Arc<dyn BundleService> = Arc::new(BundleServiceProvider(2));
        let mut conflicting = DeviceBundle::from_registration(DeviceRegistration::Device(
            Arc::new(D::new_mmio(0x2000, 0x100, "must-not-register")),
        ));
        conflicting
            .provide_service::<BundleServiceKey>(conflicting_provider)
            .unwrap();

        assert!(matches!(
            devices.register_bundle(conflicting),
            Err(crate::DeviceManagerError::ResourceConflict {
                operation: "register device service",
                ..
            })
        ));
        assert_eq!(devices.device_count(), 0);
        assert_eq!(
            devices
                .services()
                .require::<BundleServiceKey>()
                .unwrap()
                .value(),
            1
        );
    }

    #[test]
    fn runtime_invokes_registered_lifecycle_capability() {
        let lifecycle = Arc::new(CountingLifecycle {
            reset_calls: AtomicUsize::new(0),
            suspend_calls: AtomicUsize::new(0),
            resume_calls: AtomicUsize::new(0),
        });
        let bundle = DeviceBundle::new().with_lifecycle(lifecycle.clone());
        let mut devices = DeviceRuntime::empty();
        devices.register_bundle(bundle).unwrap();

        devices.reset_lifecycle_devices().unwrap();
        devices.suspend_lifecycle_devices().unwrap();
        devices.resume_lifecycle_devices().unwrap();

        assert_eq!(lifecycle.reset_calls.load(Ordering::Relaxed), 1);
        assert_eq!(lifecycle.suspend_calls.load(Ordering::Relaxed), 1);
        assert_eq!(lifecycle.resume_calls.load(Ordering::Relaxed), 1);
    }
}
