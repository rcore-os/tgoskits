//! Plain VirtIO PCI capability descriptions.
//!
//! The descriptions contain only VirtIO wire-format data.  They intentionally
//! do not mention `axdevice`'s config-space storage or layout types; the PCI
//! adapter owns that conversion.

use alloc::vec::Vec;

/// Conventional PCI vendor-specific capability identifier.
pub const VIRTIO_PCI_CAP_VENDOR_SPECIFIC: u8 = 0x09;

/// Stable effect number reserved by the VirtIO adapter for `pci_cfg_data`.
pub const VIRTIO_PCI_CONFIG_EFFECT_ID: u16 = 1;

/// VirtIO vendor capability type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum VirtioPciCapabilityType {
    /// Common configuration registers.
    Common    = 1,
    /// Queue notification window.
    Notify    = 2,
    /// Interrupt status register.
    Isr       = 3,
    /// Device-specific configuration registers.
    Device    = 4,
    /// PCI configuration access window.
    PciConfig = 5,
}

/// One serialized VirtIO vendor capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtioPciCapability {
    cfg_type: VirtioPciCapabilityType,
    bar: u8,
    offset: u32,
    length: u32,
    notify_off_multiplier: u32,
}

impl VirtioPciCapability {
    /// Creates a capability description without a notify multiplier.
    pub const fn new(cfg_type: VirtioPciCapabilityType, bar: u8, offset: u32, length: u32) -> Self {
        Self {
            cfg_type,
            bar,
            offset,
            length,
            notify_off_multiplier: 0,
        }
    }

    /// Sets the multiplier used by a notify capability.
    pub const fn with_notify_multiplier(mut self, multiplier: u32) -> Self {
        self.notify_off_multiplier = multiplier;
        self
    }

    /// Returns the VirtIO capability type.
    pub const fn cfg_type(self) -> VirtioPciCapabilityType {
        self.cfg_type
    }

    /// Returns the BAR index containing this capability's target window.
    pub const fn bar(self) -> u8 {
        self.bar
    }

    /// Returns the BAR-relative target offset.
    pub const fn offset(self) -> u32 {
        self.offset
    }

    /// Returns the target window length.
    pub const fn length(self) -> u32 {
        self.length
    }

    /// Returns the notify offset multiplier.
    pub const fn notify_off_multiplier(self) -> u32 {
        self.notify_off_multiplier
    }

    /// Returns the serialized capability length including PCI's two-byte header.
    pub const fn serialized_length(self) -> u8 {
        match self.cfg_type {
            VirtioPciCapabilityType::Notify | VirtioPciCapabilityType::PciConfig => 20,
            _ => 16,
        }
    }

    /// Returns the capability payload after PCI's two-byte header.
    pub fn body(self) -> Vec<u8> {
        let mut body = alloc::vec![0; usize::from(self.serialized_length()) - 2];
        // PCI owns the vendor ID and next-pointer bytes.  The remaining
        // VirtIO capability starts with cap_len; the three-byte padding
        // between bar and offset is part of the wire format as well.
        body[0] = self.serialized_length();
        body[1] = self.cfg_type as u8;
        body[2] = self.bar;
        body[6..10].copy_from_slice(&self.offset.to_le_bytes());
        body[10..14].copy_from_slice(&self.length.to_le_bytes());
        if matches!(
            self.cfg_type,
            VirtioPciCapabilityType::Notify | VirtioPciCapabilityType::PciConfig
        ) {
            body[14..18].copy_from_slice(&self.notify_off_multiplier.to_le_bytes());
        }
        body
    }
}

/// The standard five-capability modern VirtIO PCI declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VirtioPciCapabilitySet {
    capabilities: Vec<VirtioPciCapability>,
}

impl VirtioPciCapabilitySet {
    /// Builds common, notify, ISR, device-config and PCI_CFG capabilities in
    /// the order recommended by the VirtIO PCI transport specification.
    pub fn new(device_config_size: u32) -> Self {
        Self {
            capabilities: alloc::vec![
                VirtioPciCapability::new(VirtioPciCapabilityType::Common, 0, 0x000, 0x38),
                VirtioPciCapability::new(VirtioPciCapabilityType::Notify, 0, 0x100, 0x04)
                    .with_notify_multiplier(4),
                VirtioPciCapability::new(VirtioPciCapabilityType::Isr, 0, 0x200, 0x01),
                VirtioPciCapability::new(
                    VirtioPciCapabilityType::Device,
                    0,
                    0x300,
                    device_config_size,
                ),
                VirtioPciCapability::new(VirtioPciCapabilityType::PciConfig, 0, 0, 0),
            ],
        }
    }

    /// Returns the capabilities in deterministic serialization order.
    pub fn as_slice(&self) -> &[VirtioPciCapability] {
        &self.capabilities
    }
}
