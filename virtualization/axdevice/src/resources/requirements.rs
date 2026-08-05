//! Model-declared resource requirements.

use alloc::{format, string::String, vec::Vec};
use core::fmt;

use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptSharing, InterruptTrigger, ItsId, LpiId,
    MsiDeviceId, MsiEventId,
};

use crate::{DeviceManagerError, DeviceManagerResult};

/// A model-defined resource name such as `registers` or `irq`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ResourceSlot(String);

impl ResourceSlot {
    /// Creates a validated resource slot.
    pub fn new(value: impl Into<String>) -> DeviceManagerResult<Self> {
        validate_identifier("create resource slot", value.into()).map(Self)
    }

    /// Returns the slot name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Selects automatic allocation or a fixed ABI value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceRequest<T> {
    /// Allocate the lowest available value from the architecture auto pool.
    Auto,
    /// Require the exact supplied value from the fixed-resource allowlist.
    Fixed(T),
}

/// Compound allocation request for one MSI event/LPI range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsiResourceRequest {
    controller: InterruptControllerId,
    its: ItsId,
    count: u32,
    device: ResourceRequest<MsiDeviceId>,
    event: ResourceRequest<MsiEventId>,
    lpi: ResourceRequest<LpiId>,
}

impl MsiResourceRequest {
    /// Creates a validated MSI range request.
    pub fn new(
        controller: InterruptControllerId,
        its: ItsId,
        count: u32,
        device: ResourceRequest<MsiDeviceId>,
        event: ResourceRequest<MsiEventId>,
        lpi: ResourceRequest<LpiId>,
    ) -> DeviceManagerResult<Self> {
        if count == 0 {
            return Err(DeviceManagerError::InvalidConfig {
                operation: "declare MSI resource",
                detail: "an MSI range requires at least one event".into(),
            });
        }
        Ok(Self {
            controller,
            its,
            count,
            device,
            event,
            lpi,
        })
    }

    pub(crate) const fn controller(self) -> InterruptControllerId {
        self.controller
    }

    pub(crate) const fn its(self) -> ItsId {
        self.its
    }

    pub(crate) const fn count(self) -> u32 {
        self.count
    }

    pub(crate) const fn device(self) -> ResourceRequest<MsiDeviceId> {
        self.device
    }

    pub(crate) const fn event(self) -> ResourceRequest<MsiEventId> {
        self.event
    }

    pub(crate) const fn lpi(self) -> ResourceRequest<LpiId> {
        self.lpi
    }
}

/// One resource required by a virtual-device model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeviceRequirement {
    /// A guest MMIO window.
    Mmio {
        /// Model-defined slot.
        slot: ResourceSlot,
        /// Window size in bytes.
        size: u64,
        /// Required power-of-two alignment.
        alignment: u64,
        /// Base-address request.
        request: ResourceRequest<u64>,
    },
    /// An x86 port-I/O range.
    Pio {
        /// Model-defined slot.
        slot: ResourceSlot,
        /// Range size in bytes.
        size: u16,
        /// Required power-of-two alignment.
        alignment: u16,
        /// Base-port request.
        request: ResourceRequest<u16>,
    },
    /// One wired input on a specific virtual interrupt controller.
    WiredIrq {
        /// Model-defined slot.
        slot: ResourceSlot,
        /// Controller namespace.
        controller: InterruptControllerId,
        /// Electrical trigger semantics.
        trigger: InterruptTrigger,
        /// Planned sharing policy.
        sharing: InterruptSharing,
        /// Controller-local input request.
        request: ResourceRequest<ControllerInputId>,
    },
    /// One contiguous MSI event/LPI range in an ITS namespace.
    Msi {
        /// Model-defined slot.
        slot: ResourceSlot,
        /// Compound DeviceID/EventID/LPI request.
        request: MsiResourceRequest,
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

    pub(crate) const fn is_fixed(&self) -> bool {
        match self {
            Self::Mmio { request, .. } => matches!(request, ResourceRequest::Fixed(_)),
            Self::Pio { request, .. } => matches!(request, ResourceRequest::Fixed(_)),
            Self::WiredIrq { request, .. } => matches!(request, ResourceRequest::Fixed(_)),
            Self::Msi { request, .. } => {
                matches!(request.device, ResourceRequest::Fixed(_))
                    || matches!(request.event, ResourceRequest::Fixed(_))
                    || matches!(request.lpi, ResourceRequest::Fixed(_))
            }
        }
    }
}

/// Resource requirements declared before architecture allocation.
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

    /// Adds one MMIO requirement.
    pub fn with_mmio(
        mut self,
        slot: ResourceSlot,
        size: u64,
        alignment: u64,
        request: ResourceRequest<u64>,
    ) -> DeviceManagerResult<Self> {
        if size == 0 || !alignment.is_power_of_two() {
            return Err(invalid_requirement(
                "declare MMIO resource",
                &slot,
                "non-zero size and power-of-two alignment",
            ));
        }
        self.insert(DeviceRequirement::Mmio {
            slot,
            size,
            alignment,
            request,
        })?;
        Ok(self)
    }

    /// Adds one port-I/O requirement.
    pub fn with_pio(
        mut self,
        slot: ResourceSlot,
        size: u16,
        alignment: u16,
        request: ResourceRequest<u16>,
    ) -> DeviceManagerResult<Self> {
        if size == 0 || !alignment.is_power_of_two() {
            return Err(invalid_requirement(
                "declare PIO resource",
                &slot,
                "non-zero size and power-of-two alignment",
            ));
        }
        self.insert(DeviceRequirement::Pio {
            slot,
            size,
            alignment,
            request,
        })?;
        Ok(self)
    }

    /// Adds one wired-interrupt requirement.
    pub fn with_wired_irq(
        mut self,
        slot: ResourceSlot,
        controller: InterruptControllerId,
        trigger: InterruptTrigger,
        sharing: InterruptSharing,
        request: ResourceRequest<ControllerInputId>,
    ) -> DeviceManagerResult<Self> {
        self.insert(DeviceRequirement::WiredIrq {
            slot,
            controller,
            trigger,
            sharing,
            request,
        })?;
        Ok(self)
    }

    /// Adds one contiguous MSI event/LPI range requirement.
    pub fn with_msi(
        mut self,
        slot: ResourceSlot,
        request: MsiResourceRequest,
    ) -> DeviceManagerResult<Self> {
        self.insert(DeviceRequirement::Msi { slot, request })?;
        Ok(self)
    }

    /// Returns requirements in model declaration order.
    pub fn entries(&self) -> &[DeviceRequirement] {
        &self.entries
    }

    fn insert(&mut self, requirement: DeviceRequirement) -> DeviceManagerResult {
        if self
            .entries
            .iter()
            .any(|existing| existing.slot() == requirement.slot())
        {
            return Err(DeviceManagerError::ResourceConflict {
                operation: "declare device resources",
                detail: format!("slot {} is declared twice", requirement.slot()),
            });
        }
        self.entries.push(requirement);
        Ok(())
    }
}

/// One stable device instance and its model-declared requirements.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevicePlanRequest {
    id: String,
    requirements: DeviceRequirements,
}

impl DevicePlanRequest {
    /// Creates a device planning request.
    pub fn new(
        id: impl Into<String>,
        requirements: DeviceRequirements,
    ) -> DeviceManagerResult<Self> {
        Ok(Self {
            id: validate_identifier("create planned device identifier", id.into())?,
            requirements,
        })
    }

    /// Returns the stable device identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the model requirements.
    pub const fn requirements(&self) -> &DeviceRequirements {
        &self.requirements
    }
}

fn validate_identifier(operation: &'static str, value: String) -> DeviceManagerResult<String> {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if !valid {
        return Err(DeviceManagerError::InvalidInput {
            operation,
            detail: format!("'{value}' is not a non-empty stable identifier"),
        });
    }
    Ok(value)
}

fn invalid_requirement(
    operation: &'static str,
    slot: &ResourceSlot,
    expected: &'static str,
) -> DeviceManagerError {
    DeviceManagerError::InvalidConfig {
        operation,
        detail: format!("slot {slot} requires {expected}"),
    }
}
