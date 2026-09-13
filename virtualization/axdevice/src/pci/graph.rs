//! Typed declarations connecting PCI functions to the unified device graph.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};
use core::fmt;

use axdevice_base::{ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger};

use super::{
    PciBdf, PciCapabilitySpec, PciEndpointIdentity, PciError, PciFunctionSpec, PciMemoryBar,
    PciResult, config_layout,
};
use crate::{DeviceManagerError, DeviceNodeId, DeviceNodeSpec, ResourceRequest, ResourceSlot};

/// Conventional PCI legacy interrupt pin.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PciIntxPin {
    /// PCI INTA#.
    A,
    /// PCI INTB#.
    B,
    /// PCI INTC#.
    C,
    /// PCI INTD#.
    D,
}

impl PciIntxPin {
    /// Returns the zero-based ordinal used by PCI swizzling.
    pub const fn ordinal(self) -> u8 {
        match self {
            Self::A => 0,
            Self::B => 1,
            Self::C => 2,
            Self::D => 3,
        }
    }

    /// Returns the conventional PCI configuration-space encoding.
    pub const fn config_encoding(self) -> u8 {
        self.ordinal() + 1
    }
}

/// Endpoint-owned logical INTx attachment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciIntxRequirement {
    pin: PciIntxPin,
    slot: ResourceSlot,
}

impl PciIntxRequirement {
    /// Creates an INTx attachment consumed from the endpoint build context.
    pub fn new(pin: PciIntxPin, slot: ResourceSlot) -> Self {
        Self { pin, slot }
    }

    /// Returns the logical PCI pin.
    pub const fn pin(&self) -> PciIntxPin {
        self.pin
    }

    /// Returns the endpoint-owned IRQ resource slot.
    pub const fn slot(&self) -> &ResourceSlot {
        &self.slot
    }
}

/// Architecture-owned policy for resolving conventional PCI INTx routes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciIntxRouter {
    controller: InterruptControllerId,
    controller_dependency: Option<DeviceNodeId>,
    root_inputs: [ControllerInputId; 4],
    guest_line: [u32; 4],
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
}

impl PciIntxRouter {
    /// Creates a router with four root-bus inputs and guest-visible line IDs.
    pub const fn new(
        controller: InterruptControllerId,
        root_inputs: [ControllerInputId; 4],
        guest_line: [u32; 4],
        trigger: InterruptTrigger,
        sharing: InterruptSharing,
    ) -> Self {
        Self {
            controller,
            controller_dependency: None,
            root_inputs,
            guest_line,
            trigger,
            sharing,
        }
    }

    /// Requires the PCI host graph node to be constructed after its interrupt
    /// controller node.
    pub fn with_controller_dependency(mut self, dependency: DeviceNodeId) -> Self {
        self.controller_dependency = Some(dependency);
        self
    }

    pub(crate) const fn controller_dependency(&self) -> Option<&DeviceNodeId> {
        self.controller_dependency.as_ref()
    }

    pub(crate) fn resolve(
        &self,
        function: &DeviceNodeId,
        bdf: PciBdf,
        pin: PciIntxPin,
    ) -> PciResult<ResolvedPciIntx> {
        if bdf.bus() != 0 || bdf.function() != 0 {
            return Err(PciError::IntxRouteUnavailable {
                function: function.to_string(),
                detail: "the conventional root-bus INTx router only supports bus 0 function 0"
                    .into(),
            });
        }
        let root_pin = usize::from((bdf.device() + pin.ordinal()) % 4);
        Ok(ResolvedPciIntx {
            pin,
            controller: self.controller,
            input: self.root_inputs[root_pin],
            trigger: self.trigger,
            sharing: self.sharing,
            guest_line: self.guest_line[root_pin],
        })
    }
}

#[cfg(test)]
mod tests {
    use axdevice_base::{
        ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger,
    };

    use super::*;
    use crate::{DeviceNodeId, PciSegment};

    #[test]
    fn conventional_swizzle_maps_all_pins_and_rejects_non_root_buses() {
        let router = PciIntxRouter::new(
            InterruptControllerId::new(0),
            [
                ControllerInputId::new(16),
                ControllerInputId::new(17),
                ControllerInputId::new(18),
                ControllerInputId::new(19),
            ],
            [16, 17, 18, 19],
            InterruptTrigger::LevelTriggered,
            InterruptSharing::Shared,
        );
        let function = DeviceNodeId::new("endpoint").unwrap();
        for (pin, input) in [
            (PciIntxPin::A, 17),
            (PciIntxPin::B, 18),
            (PciIntxPin::C, 19),
            (PciIntxPin::D, 16),
        ] {
            assert_eq!(
                router
                    .resolve(&function, PciBdf::bus_zero(1), pin)
                    .unwrap()
                    .input(),
                ControllerInputId::new(input)
            );
        }
        assert!(matches!(
            router.resolve(
                &function,
                PciBdf::new(PciSegment::new(0), 1, 1, 0).unwrap(),
                PciIntxPin::A,
            ),
            Err(PciError::IntxRouteUnavailable { .. })
        ));
    }
}

/// Resolved endpoint INTx route shared by runtime, config space and firmware.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedPciIntx {
    pin: PciIntxPin,
    controller: InterruptControllerId,
    input: ControllerInputId,
    trigger: InterruptTrigger,
    sharing: InterruptSharing,
    guest_line: u32,
}

impl ResolvedPciIntx {
    /// Returns the endpoint's logical pin.
    pub const fn pin(self) -> PciIntxPin {
        self.pin
    }

    /// Returns the interrupt controller namespace.
    pub const fn controller(self) -> InterruptControllerId {
        self.controller
    }

    /// Returns the controller-local input.
    pub const fn input(self) -> ControllerInputId {
        self.input
    }

    /// Returns the electrical trigger contract.
    pub const fn trigger(self) -> InterruptTrigger {
        self.trigger
    }

    /// Returns the sharing contract.
    pub const fn sharing(self) -> InterruptSharing {
        self.sharing
    }

    /// Returns the guest-visible Interrupt Line value before byte encoding.
    pub const fn guest_line(self) -> u32 {
        self.guest_line
    }

    /// Returns the PCI configuration-space Interrupt Line byte.
    pub const fn guest_line_byte(self) -> u8 {
        if self.guest_line <= u8::MAX as u32 {
            self.guest_line as u8
        } else {
            u8::MAX
        }
    }
}

/// Stable key selecting one architecture-provided PCI host.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PciHostKey(String);

impl PciHostKey {
    /// Creates a validated host key.
    pub fn new(value: impl Into<String>) -> Result<Self, DeviceManagerError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@')
            });
        if !valid {
            return Err(DeviceManagerError::InvalidInput {
                operation: "create PCI host key",
                detail: alloc::format!("'{value}' is not a stable PCI host key"),
            });
        }
        Ok(Self(value))
    }

    /// Returns the stable textual key.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PciHostKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One ordinary model's request to appear as a PCI function.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PciFunctionRequirement {
    pub(crate) host: PciHostKey,
    pub(crate) identity: PciEndpointIdentity,
    pub(crate) bdf: ResourceRequest<PciBdf>,
    pub(crate) bars: Vec<PciMemoryBar>,
    pub(crate) capabilities: Vec<PciCapabilitySpec>,
    pub(crate) intx: Option<PciIntxRequirement>,
}

impl PciFunctionRequirement {
    /// Creates an automatically placed function with no BARs.
    pub fn new(host: PciHostKey, identity: PciEndpointIdentity) -> Self {
        Self {
            host,
            identity,
            bdf: ResourceRequest::Auto,
            bars: Vec::new(),
            capabilities: Vec::new(),
            intx: None,
        }
    }

    /// Selects automatic or fixed BDF placement.
    pub const fn with_bdf(mut self, bdf: ResourceRequest<PciBdf>) -> Self {
        self.bdf = bdf;
        self
    }

    /// Adds one memory BAR.
    pub fn with_bar(mut self, bar: PciMemoryBar) -> PciResult<Self> {
        if self
            .bars
            .iter()
            .any(|existing| existing.index() == bar.index())
        {
            return Err(PciError::InvalidBar {
                bar: bar.index(),
                detail: "BAR slot is already occupied by this function".into(),
            });
        }
        self.bars.push(bar);
        Ok(self)
    }

    /// Adds one conventional PCI capability declaration.
    pub fn with_capability(mut self, capability: PciCapabilitySpec) -> Self {
        self.capabilities.push(capability);
        self
    }

    /// Attaches one endpoint-owned conventional INTx requirement.
    pub fn with_intx(mut self, intx: PciIntxRequirement) -> PciResult<Self> {
        if self.intx.is_some() {
            return Err(PciError::InvalidConfigPatch {
                offset: config_layout::CONFIG_INTERRUPT_PIN_OFFSET as u16,
                detail: "a PCI function may declare at most one INTx attachment",
            });
        }
        self.intx = Some(intx);
        Ok(self)
    }

    /// Returns the selected host key.
    pub const fn host(&self) -> &PciHostKey {
        &self.host
    }

    /// Returns the optional endpoint-owned INTx attachment.
    pub const fn intx(&self) -> Option<&PciIntxRequirement> {
        self.intx.as_ref()
    }

    pub(crate) fn function_spec(&self, id: DeviceNodeId) -> PciResult<PciFunctionSpec> {
        let mut spec = PciFunctionSpec::new(id, self.identity).with_bdf(self.bdf);
        for bar in &self.bars {
            spec = spec.with_bar(bar.clone())?;
        }
        for capability in &self.capabilities {
            spec = spec.with_capability(capability.clone());
        }
        if let Some(intx) = &self.intx {
            spec = spec.with_intx(intx.clone())?;
        }
        Ok(spec)
    }
}

/// Architecture-owned description of one PCI host graph node.
pub struct PciHostProvider {
    pub(crate) key: PciHostKey,
    pub(crate) node: DeviceNodeSpec,
    pub(crate) memory_aperture_slot: ResourceSlot,
    pub(crate) platform_functions: Vec<PciFunctionSpec>,
    pub(crate) reserved_bdfs: Vec<PciBdf>,
    pub(crate) intx_router: Option<PciIntxRouter>,
}

impl PciHostProvider {
    /// Creates a provider backed by an ordinary graph node and MMIO slot.
    pub fn new(key: PciHostKey, node: DeviceNodeSpec, memory_aperture_slot: ResourceSlot) -> Self {
        Self {
            key,
            node,
            memory_aperture_slot,
            platform_functions: Vec::new(),
            reserved_bdfs: Vec::new(),
            intx_router: None,
        }
    }

    /// Adds one platform-owned fixed function.
    pub fn with_platform_function(mut self, function: PciFunctionSpec) -> PciResult<Self> {
        if self
            .platform_functions
            .iter()
            .any(|existing| existing.id() == function.id())
        {
            return Err(PciError::DuplicateFunction {
                function: function.id().to_string(),
            });
        }
        self.platform_functions.push(function);
        Ok(self)
    }

    /// Reserves one BDF from endpoint allocation.
    pub fn with_reserved_bdf(mut self, bdf: PciBdf) -> Self {
        self.reserved_bdfs.push(bdf);
        self
    }

    /// Installs the architecture-owned conventional INTx routing policy.
    pub fn with_intx_router(mut self, router: PciIntxRouter) -> Self {
        self.intx_router = Some(router);
        self
    }
}
