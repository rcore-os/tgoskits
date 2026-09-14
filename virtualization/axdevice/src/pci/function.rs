//! Type-0 PCI function identity and topology declarations.

use alloc::vec::Vec;

use super::{
    PciCapabilitySpec, PciError, PciIntxRequirement, PciMemoryBar, PciResult, config_layout,
};
use crate::{ConfigOffset, DeviceNodeId, ResourceRequest};

#[derive(Clone, Copy, Debug)]
pub(crate) struct PciConfigByte {
    pub(crate) offset: ConfigOffset,
    pub(crate) value: u8,
    pub(crate) write_mask: u8,
}

/// PCI class-code triplet for a Type-0 function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciClass {
    base: u8,
    subclass: u8,
    programming_interface: u8,
}

impl PciClass {
    /// Creates a class-code triplet.
    pub const fn new(base: u8, subclass: u8, programming_interface: u8) -> Self {
        Self {
            base,
            subclass,
            programming_interface,
        }
    }

    /// Returns the base class.
    pub const fn base(self) -> u8 {
        self.base
    }

    /// Returns the subclass.
    pub const fn subclass(self) -> u8 {
        self.subclass
    }

    /// Returns the programming interface.
    pub const fn programming_interface(self) -> u8 {
        self.programming_interface
    }
}

/// Immutable identity fields of one Type-0 PCI function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciEndpointIdentity {
    vendor_id: u16,
    device_id: u16,
    subsystem_vendor_id: u16,
    subsystem_device_id: u16,
    class: PciClass,
    revision: u8,
}

impl PciEndpointIdentity {
    /// Creates an endpoint identity with revision zero.
    pub const fn new(vendor_id: u16, device_id: u16, class: PciClass) -> Self {
        Self {
            vendor_id,
            device_id,
            subsystem_vendor_id: 0,
            subsystem_device_id: 0,
            class,
            revision: 0,
        }
    }

    /// Sets the revision ID.
    pub const fn with_revision(mut self, revision: u8) -> Self {
        self.revision = revision;
        self
    }

    /// Sets the immutable subsystem identity fields.
    pub const fn with_subsystem_ids(mut self, vendor_id: u16, device_id: u16) -> Self {
        self.subsystem_vendor_id = vendor_id;
        self.subsystem_device_id = device_id;
        self
    }

    /// Returns the vendor ID.
    pub const fn vendor_id(self) -> u16 {
        self.vendor_id
    }

    /// Returns the device ID.
    pub const fn device_id(self) -> u16 {
        self.device_id
    }

    /// Returns the subsystem vendor ID.
    pub const fn subsystem_vendor_id(self) -> u16 {
        self.subsystem_vendor_id
    }

    /// Returns the subsystem device ID.
    pub const fn subsystem_device_id(self) -> u16 {
        self.subsystem_device_id
    }

    /// Returns the class-code triplet.
    pub const fn class(self) -> PciClass {
        self.class
    }

    /// Returns the revision ID.
    pub const fn revision(self) -> u8 {
        self.revision
    }
}

/// Unresolved Type-0 function declaration consumed during graph PCI resolution.
#[derive(Clone, Debug)]
pub struct PciFunctionSpec {
    pub(crate) id: DeviceNodeId,
    pub(crate) identity: PciEndpointIdentity,
    pub(crate) bdf: ResourceRequest<super::PciBdf>,
    pub(crate) bars: Vec<PciMemoryBar>,
    pub(crate) capabilities: Vec<PciCapabilitySpec>,
    pub(crate) intx: Option<PciIntxRequirement>,
    pub(crate) config_bytes: Vec<PciConfigByte>,
}

impl PciFunctionSpec {
    /// Creates an automatically placed function with no BARs.
    pub fn new(id: DeviceNodeId, identity: PciEndpointIdentity) -> Self {
        Self {
            id,
            identity,
            bdf: ResourceRequest::Auto,
            bars: Vec::new(),
            capabilities: Vec::new(),
            intx: None,
            config_bytes: Vec::new(),
        }
    }

    /// Returns the stable function identity.
    pub const fn id(&self) -> &DeviceNodeId {
        &self.id
    }

    /// Selects automatic or fixed BDF placement.
    pub const fn with_bdf(mut self, bdf: ResourceRequest<super::PciBdf>) -> Self {
        self.bdf = bdf;
        self
    }

    /// Adds one memory BAR.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidBar`] if this function already uses the BAR
    /// slot.
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
    pub(crate) fn with_intx(mut self, intx: PciIntxRequirement) -> PciResult<Self> {
        if self.intx.is_some() {
            return Err(PciError::InvalidConfigPatch {
                offset: config_layout::CONFIG_INTERRUPT_PIN_OFFSET as u16,
                detail: "a PCI function may declare at most one INTx attachment",
            });
        }
        self.intx = Some(intx);
        Ok(self)
    }

    /// Defines one platform-owned conventional config byte and its write mask.
    ///
    /// This is intended for fixed host-bridge compatibility fields. BAR bytes
    /// cannot be overridden because their state is owned by the root BAR
    /// state machine.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidConfigPatch`] when the offset belongs to
    /// core identity, status, or BAR state, or when the byte is duplicated.
    pub fn with_platform_config_byte(
        mut self,
        offset: ConfigOffset,
        value: u8,
        write_mask: u8,
    ) -> PciResult<Self> {
        let offset_value = offset.value();
        let offset_usize = usize::from(offset_value);
        if (config_layout::CONFIG_BAR_START..config_layout::CONFIG_BAR_END).contains(&offset_usize)
        {
            return Err(PciError::InvalidConfigPatch {
                offset: offset_value,
                detail: "BAR bytes are owned by the BAR state machine",
            });
        }
        if offset_usize != config_layout::CONFIG_HEADER_TYPE_OFFSET
            && offset_usize < config_layout::CONFIG_STANDARD_HEADER_END
        {
            return Err(PciError::InvalidConfigPatch {
                offset: offset_value,
                detail: "core identity, command, and status fields cannot be overridden",
            });
        }
        if self
            .config_bytes
            .iter()
            .any(|existing| existing.offset == offset)
        {
            return Err(PciError::InvalidConfigPatch {
                offset: offset.value(),
                detail: "config byte is already defined",
            });
        }
        self.config_bytes.push(PciConfigByte {
            offset,
            value,
            write_mask,
        });
        Ok(self)
    }
}
