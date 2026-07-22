// Copyright 2026 The Axvisor Team
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

//! Two-phase virtual-device model contracts.

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::cell::RefCell;

use axdevice_base::{
    ControllerInputId, InterruptSharing, InterruptTriggerMode, IrqLine, MsiDeviceId, MsiEndpoint,
    MsiEventId,
};

use crate::{
    DeviceBundle, DeviceManagerError, DeviceManagerResult, DeviceRegistration,
    InterruptEndpointRegistration, InterruptPlanAuthority, InterruptTopology, MsiRequest,
    WiredIrqRequest,
};

/// VM-owned capabilities available while one planned device is constructed.
pub struct DeviceBuildContext<'a> {
    interrupt_topology: &'a InterruptTopology,
    interrupt_authority: &'a InterruptPlanAuthority,
    resources: &'a ResolvedDeviceResources,
    backend: DeviceBackend,
    opened_interrupts: RefCell<BTreeMap<ResourceSlot, OpenedInterrupt>>,
}

enum OpenedInterrupt {
    Wired {
        line: IrqLine,
        registration: InterruptEndpointRegistration,
    },
    Msi {
        endpoint: MsiEndpoint,
        registration: InterruptEndpointRegistration,
    },
}

impl<'a> DeviceBuildContext<'a> {
    /// Creates a context scoped to one device's resolved resources.
    pub fn new(
        interrupt_topology: &'a InterruptTopology,
        interrupt_authority: &'a InterruptPlanAuthority,
        resources: &'a ResolvedDeviceResources,
    ) -> Self {
        Self {
            interrupt_topology,
            interrupt_authority,
            resources,
            backend: DeviceBackend::None,
            opened_interrupts: RefCell::new(BTreeMap::new()),
        }
    }

    /// Creates a context with one planner-selected external backend policy.
    pub fn with_backend(
        interrupt_topology: &'a InterruptTopology,
        interrupt_authority: &'a InterruptPlanAuthority,
        resources: &'a ResolvedDeviceResources,
        backend: DeviceBackend,
    ) -> Self {
        Self {
            interrupt_topology,
            interrupt_authority,
            resources,
            backend,
            opened_interrupts: RefCell::new(BTreeMap::new()),
        }
    }

    /// Returns the external backend policy selected for this device instance.
    pub const fn backend(&self) -> DeviceBackend {
        self.backend
    }

    /// Opens the named wired interrupt allocated to this device.
    pub fn irq(&self, slot: &ResourceSlot) -> DeviceManagerResult<IrqLine> {
        if let Some(opened) = self.opened_interrupts.borrow().get(slot) {
            return match opened {
                OpenedInterrupt::Wired { line, .. } => Ok(line.clone()),
                OpenedInterrupt::Msi { .. } => Err(resource_kind_error(slot, "wired IRQ")),
            };
        }
        let claim = self
            .interrupt_authority
            .claim_wired(self.interrupt_topology, self.resources.wired_irq(slot)?)?;
        let (line, registration) = self.interrupt_topology.connect_irq(claim)?.into_parts();
        self.opened_interrupts.borrow_mut().insert(
            slot.clone(),
            OpenedInterrupt::Wired {
                line: line.clone(),
                registration,
            },
        );
        Ok(line)
    }

    /// Opens the named message-signaled endpoint allocated to this device.
    pub fn msi(&self, slot: &ResourceSlot) -> DeviceManagerResult<MsiEndpoint> {
        if let Some(opened) = self.opened_interrupts.borrow().get(slot) {
            return match opened {
                OpenedInterrupt::Msi { endpoint, .. } => Ok(endpoint.clone()),
                OpenedInterrupt::Wired { .. } => Err(resource_kind_error(slot, "MSI")),
            };
        }
        let claim = self
            .interrupt_authority
            .claim_msi(self.interrupt_topology, self.resources.msi(slot)?)?;
        let (endpoint, registration) = self.interrupt_topology.connect_msi(claim)?.into_parts();
        self.opened_interrupts.borrow_mut().insert(
            slot.clone(),
            OpenedInterrupt::Msi {
                endpoint: endpoint.clone(),
                registration,
            },
        );
        Ok(endpoint)
    }

    fn finish(self, mut bundle: DeviceBundle) -> DeviceManagerResult<DeviceBundle> {
        let opened = self.opened_interrupts.into_inner();
        for (slot, resource) in &self.resources.entries {
            if matches!(
                resource,
                ResolvedResource::WiredIrq(_) | ResolvedResource::Msi(_)
            ) && !opened.contains_key(slot)
            {
                return Err(DeviceManagerError::ResourceNotFound {
                    operation: "finish planned device build",
                    resource: alloc::format!("unconsumed interrupt slot {slot}"),
                });
            }
        }
        for endpoint in opened.into_values() {
            let registration = match endpoint {
                OpenedInterrupt::Wired { registration, .. }
                | OpenedInterrupt::Msi { registration, .. } => registration,
            };
            bundle.push(DeviceRegistration::InterruptEndpoint(registration));
        }
        Ok(bundle)
    }
}

/// A stable virtual-device model name, such as `arm-pl011`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeviceModelId(String);

impl DeviceModelId {
    /// Validates and creates a model identifier.
    pub fn new(value: impl Into<String>) -> DeviceManagerResult<Self> {
        validate_name("create device model identifier", value.into()).map(Self)
    }

    /// Returns the model identifier as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for DeviceModelId {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A model-defined name for one resource, such as `registers` or `irq`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ResourceSlot(String);

impl ResourceSlot {
    /// Validates and creates a resource slot.
    pub fn new(value: impl Into<String>) -> DeviceManagerResult<Self> {
        validate_name("create device resource slot", value.into()).map(Self)
    }

    /// Returns the slot name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Display for ResourceSlot {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Host-console receive capability granted to one virtual serial device.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConsoleRxPolicy {
    /// This device owns host-console input for its lifetime.
    Exclusive,
    /// This device receives no host-console input.
    #[default]
    Disabled,
}

/// Host-console transmit capability granted to one virtual serial device.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConsoleTxPolicy {
    /// Serialize output with other shared host-console writers.
    Shared,
    /// This device owns host-console output for its lifetime.
    Exclusive,
    /// Discard bytes transmitted by the guest.
    #[default]
    Disabled,
}

/// Host-console policies carried from machine planning into device build.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostConsoleBackend {
    rx: ConsoleRxPolicy,
    tx: ConsoleTxPolicy,
}

impl HostConsoleBackend {
    /// Creates a host-console capability with explicit input and output policy.
    pub const fn new(rx: ConsoleRxPolicy, tx: ConsoleTxPolicy) -> Self {
        Self { rx, tx }
    }

    /// Returns the receive ownership policy.
    pub const fn rx(self) -> ConsoleRxPolicy {
        self.rx
    }

    /// Returns the transmit ownership policy.
    pub const fn tx(self) -> ConsoleTxPolicy {
        self.tx
    }
}

/// External capability selected for one resolved virtual-device instance.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DeviceBackend {
    /// The model has no external input or output capability.
    #[default]
    None,
    /// Connect a serial device to the hypervisor host console.
    HostConsole(HostConsoleBackend),
}

/// One resource requested by a virtual-device model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceRequirement {
    /// An MMIO range with a byte size and power-of-two alignment.
    Mmio {
        /// Model-defined resource name.
        slot: ResourceSlot,
        /// Required range size in bytes.
        size: u64,
        /// Required base-address alignment.
        alignment: u64,
    },
    /// A port-I/O range with a byte size and power-of-two alignment.
    Pio {
        /// Model-defined resource name.
        slot: ResourceSlot,
        /// Required range size in bytes.
        size: u16,
        /// Required base-port alignment.
        alignment: u16,
    },
    /// A wired interrupt input.
    WiredIrq {
        /// Model-defined resource name.
        slot: ResourceSlot,
        /// Required trigger semantics.
        trigger: InterruptTriggerMode,
        /// Whether independently owned devices may share the input.
        sharing: InterruptSharing,
    },
    /// One message-signaled interrupt event.
    Msi {
        /// Model-defined resource name.
        slot: ResourceSlot,
    },
}

impl DeviceRequirement {
    /// Returns the model-defined resource slot.
    pub const fn slot(&self) -> &ResourceSlot {
        match self {
            Self::Mmio { slot, .. }
            | Self::Pio { slot, .. }
            | Self::WiredIrq { slot, .. }
            | Self::Msi { slot, .. } => slot,
        }
    }
}

/// Resource requirements declared before VM address and interrupt allocation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceRequirements {
    entries: Vec<DeviceRequirement>,
}

impl DeviceRequirements {
    /// Creates an empty requirement set.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Adds one MMIO resource requirement.
    pub fn with_mmio(
        mut self,
        slot: ResourceSlot,
        size: u64,
        alignment: u64,
    ) -> DeviceManagerResult<Self> {
        if size == 0 || !alignment.is_power_of_two() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "declare device MMIO requirement",
                detail: alloc::format!(
                    "slot {slot} requires non-zero size and power-of-two alignment"
                ),
            });
        }
        self.insert(DeviceRequirement::Mmio {
            slot,
            size,
            alignment,
        })?;
        Ok(self)
    }

    /// Adds one port-I/O resource requirement.
    pub fn with_pio(
        mut self,
        slot: ResourceSlot,
        size: u16,
        alignment: u16,
    ) -> DeviceManagerResult<Self> {
        if size == 0 || !alignment.is_power_of_two() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "declare device PIO requirement",
                detail: alloc::format!(
                    "slot {slot} requires non-zero size and power-of-two alignment"
                ),
            });
        }
        self.insert(DeviceRequirement::Pio {
            slot,
            size,
            alignment,
        })?;
        Ok(self)
    }

    /// Adds one wired-interrupt requirement.
    pub fn with_wired_irq(
        mut self,
        slot: ResourceSlot,
        trigger: InterruptTriggerMode,
        sharing: InterruptSharing,
    ) -> DeviceManagerResult<Self> {
        self.insert(DeviceRequirement::WiredIrq {
            slot,
            trigger,
            sharing,
        })?;
        Ok(self)
    }

    /// Adds one MSI requirement.
    pub fn with_msi(mut self, slot: ResourceSlot) -> DeviceManagerResult<Self> {
        self.insert(DeviceRequirement::Msi { slot })?;
        Ok(self)
    }

    /// Returns all requirements in declaration order.
    pub fn entries(&self) -> &[DeviceRequirement] {
        &self.entries
    }

    fn insert(&mut self, requirement: DeviceRequirement) -> DeviceManagerResult {
        if self
            .entries
            .iter()
            .any(|entry| entry.slot() == requirement.slot())
        {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "declare device requirements",
                detail: alloc::format!("resource slot {} is declared twice", requirement.slot()),
            });
        }
        self.entries.push(requirement);
        Ok(())
    }
}

/// Firmware properties inherited from a host node or supplied by a profile.
///
/// The model may inspect this data while declaring requirements, but resource
/// addresses and interrupt routing are resolved separately by the VM planner.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DeviceTemplate {
    compatible: Vec<String>,
}

impl DeviceTemplate {
    /// Creates a template from compatible strings in firmware preference order.
    pub fn new(compatible: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            compatible: compatible.into_iter().map(Into::into).collect(),
        }
    }

    /// Returns the compatible strings in firmware preference order.
    pub fn compatible(&self) -> &[String] {
        &self.compatible
    }

    /// Returns whether this template advertises `compatible`.
    pub fn has_compatible(&self, compatible: &str) -> bool {
        self.compatible
            .iter()
            .any(|candidate| candidate == compatible)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ResolvedResource {
    Mmio { base: u64, size: u64 },
    Pio { base: u16, size: u16 },
    WiredIrq(WiredIrqRequest),
    Msi(MsiRequest),
}

/// Named resources allocated by the VM machine planner before a model is
/// allowed to construct its runtime device.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ResolvedDeviceResources {
    entries: BTreeMap<ResourceSlot, ResolvedResource>,
}

impl ResolvedDeviceResources {
    /// Creates an empty resource set.
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Adds one resolved MMIO range.
    pub fn with_mmio(
        mut self,
        slot: ResourceSlot,
        base: u64,
        size: u64,
    ) -> DeviceManagerResult<Self> {
        if size == 0 || base.checked_add(size).is_none() {
            return Err(DeviceManagerError::InvalidInput {
                operation: "resolve device MMIO resource",
                detail: alloc::format!("slot {slot} has an empty or overflowing range"),
            });
        }
        self.insert(slot, ResolvedResource::Mmio { base, size })?;
        Ok(self)
    }

    /// Adds one resolved port-I/O range.
    pub fn with_pio(
        mut self,
        slot: ResourceSlot,
        base: u16,
        size: u16,
    ) -> DeviceManagerResult<Self> {
        if size == 0 || u32::from(base) + u32::from(size) > 0x1_0000 {
            return Err(DeviceManagerError::InvalidInput {
                operation: "resolve device PIO resource",
                detail: alloc::format!("slot {slot} has an empty or overflowing range"),
            });
        }
        self.insert(slot, ResolvedResource::Pio { base, size })?;
        Ok(self)
    }

    /// Adds one wired input on the topology's default controller.
    pub fn with_wired_irq(
        self,
        slot: ResourceSlot,
        input: ControllerInputId,
        trigger: InterruptTriggerMode,
        sharing: InterruptSharing,
    ) -> DeviceManagerResult<Self> {
        self.with_wired_irq_request(slot, WiredIrqRequest::new(input, trigger, sharing))
    }

    /// Adds one planner-selected wired-controller connection.
    pub fn with_wired_irq_request(
        mut self,
        slot: ResourceSlot,
        request: WiredIrqRequest,
    ) -> DeviceManagerResult<Self> {
        self.insert(slot, ResolvedResource::WiredIrq(request))?;
        Ok(self)
    }

    /// Adds one MSI event on the topology's default controller.
    pub fn with_msi(
        self,
        slot: ResourceSlot,
        device: MsiDeviceId,
        event: MsiEventId,
    ) -> DeviceManagerResult<Self> {
        self.with_msi_request(slot, MsiRequest::new(device, event))
    }

    /// Adds one planner-selected MSI-controller connection.
    pub fn with_msi_request(
        mut self,
        slot: ResourceSlot,
        request: MsiRequest,
    ) -> DeviceManagerResult<Self> {
        self.insert(slot, ResolvedResource::Msi(request))?;
        Ok(self)
    }

    /// Returns a resolved MMIO range.
    pub fn mmio(&self, slot: &ResourceSlot) -> DeviceManagerResult<(u64, u64)> {
        match self.resource(slot, "resolve device MMIO resource")? {
            ResolvedResource::Mmio { base, size } => Ok((*base, *size)),
            _ => Err(resource_kind_error(slot, "MMIO")),
        }
    }

    /// Returns a resolved port-I/O range.
    pub fn pio(&self, slot: &ResourceSlot) -> DeviceManagerResult<(u16, u16)> {
        match self.resource(slot, "resolve device PIO resource")? {
            ResolvedResource::Pio { base, size } => Ok((*base, *size)),
            _ => Err(resource_kind_error(slot, "PIO")),
        }
    }

    pub(crate) fn wired_irq(&self, slot: &ResourceSlot) -> DeviceManagerResult<WiredIrqRequest> {
        match self.resource(slot, "connect device IRQ")? {
            ResolvedResource::WiredIrq(request) => Ok(*request),
            _ => Err(resource_kind_error(slot, "wired IRQ")),
        }
    }

    pub(crate) fn msi(&self, slot: &ResourceSlot) -> DeviceManagerResult<MsiRequest> {
        match self.resource(slot, "connect device MSI")? {
            ResolvedResource::Msi(request) => Ok(*request),
            _ => Err(resource_kind_error(slot, "MSI")),
        }
    }

    fn insert(&mut self, slot: ResourceSlot, resource: ResolvedResource) -> DeviceManagerResult {
        if self.entries.contains_key(&slot) {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "resolve device resources",
                detail: alloc::format!("resource slot {slot} is resolved twice"),
            });
        }
        self.entries.insert(slot, resource);
        Ok(())
    }

    fn resource(
        &self,
        slot: &ResourceSlot,
        operation: &'static str,
    ) -> DeviceManagerResult<&ResolvedResource> {
        self.entries
            .get(slot)
            .ok_or_else(|| DeviceManagerError::ResourceNotFound {
                operation,
                resource: slot.to_string(),
            })
    }
}

/// A two-phase virtual-device implementation.
pub trait VirtualDeviceModel: Send + Sync {
    /// Returns the stable model identifier used by VM configuration.
    fn model_id(&self) -> DeviceModelId;

    /// Returns whether a host firmware template can seed this model.
    ///
    /// Models that do not support host-derived replacement keep the default
    /// `false` result and are dynamically allocated for `source = "auto"`.
    fn matches_template(&self, _template: &DeviceTemplate) -> bool {
        false
    }

    /// Declares resources before the VM planner assigns addresses and IRQs.
    fn requirements(
        &self,
        template: Option<&DeviceTemplate>,
    ) -> DeviceManagerResult<DeviceRequirements>;

    /// Builds a runtime device using only resolved, named resources.
    fn build(
        &self,
        resources: &ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle>;
}

/// A registry containing at most one implementation for each model ID.
#[derive(Default)]
pub struct VirtualDeviceModelRegistry {
    models: BTreeMap<DeviceModelId, Arc<dyn VirtualDeviceModel>>,
}

impl VirtualDeviceModelRegistry {
    /// Creates an empty model registry.
    pub const fn new() -> Self {
        Self {
            models: BTreeMap::new(),
        }
    }

    /// Registers a model, rejecting a duplicate identifier.
    pub fn register(&mut self, model: Arc<dyn VirtualDeviceModel>) -> DeviceManagerResult {
        let id = model.model_id();
        if self.models.contains_key(&id) {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "register virtual device model",
                detail: alloc::format!("model {id} is already registered"),
            });
        }
        self.models.insert(id, model);
        Ok(())
    }

    /// Returns the model registered for `id`.
    pub fn get(&self, id: &DeviceModelId) -> Option<&dyn VirtualDeviceModel> {
        self.models.get(id).map(Arc::as_ref)
    }

    /// Declares requirements for one configured model.
    pub fn requirements(
        &self,
        id: &DeviceModelId,
        template: Option<&DeviceTemplate>,
    ) -> DeviceManagerResult<DeviceRequirements> {
        self.model(id, "declare virtual device requirements")?
            .requirements(template)
    }

    /// Builds one configured model from planner-resolved resources.
    pub fn build(
        &self,
        id: &DeviceModelId,
        resources: &ResolvedDeviceResources,
        context: DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let bundle = self
            .model(id, "build virtual device")?
            .build(resources, &context)?;
        context.finish(bundle)
    }

    fn model(
        &self,
        id: &DeviceModelId,
        operation: &'static str,
    ) -> DeviceManagerResult<&dyn VirtualDeviceModel> {
        self.get(id).ok_or_else(|| DeviceManagerError::Unsupported {
            operation,
            detail: alloc::format!("no implementation is registered for model {id}"),
        })
    }
}

fn validate_name(operation: &'static str, value: String) -> DeviceManagerResult<String> {
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    {
        return Err(DeviceManagerError::InvalidInput {
            operation,
            detail: alloc::format!("'{value}' is not a valid identifier"),
        });
    }
    Ok(value)
}

fn resource_kind_error(slot: &ResourceSlot, expected: &'static str) -> DeviceManagerError {
    DeviceManagerError::InvalidInput {
        operation: "consume resolved device resource",
        detail: alloc::format!("slot {slot} is not a resolved {expected} resource"),
    }
}
