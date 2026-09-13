//! Non-VHE, 4-KiB guest translation geometry and root encoding.

use crate::{PhysAddr, virtualization::VirtualizationError};

/// A stage-two root and VTCR configuration using the implemented descriptor format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Stage2Config {
    root: PhysAddr,
    control: u64,
}

impl Stage2Config {
    /// Validates a three/four-level root, IPA width and physical output width.
    /// This constructor does not access hardware; the owner must choose an
    /// output width supported by every CPU that may run this guest.
    pub fn new(
        root: PhysAddr,
        levels: usize,
        ipa_bits: usize,
        physical_bits: usize,
    ) -> Result<Self, VirtualizationError> {
        let sl0 = match (levels, ipa_bits) {
            (3, 31..=39) => 1,
            (4, 40..=48) => 2,
            _ => return Err(VirtualizationError::UnsupportedPaging),
        };
        let ps = match physical_bits {
            32 => 0,
            36 => 1,
            40 => 2,
            42 => 3,
            44 => 4,
            48 => 5,
            _ => return Err(VirtualizationError::UnsupportedPaging),
        };
        let address = root.as_usize();
        if address & 4095 != 0 || address > (1usize << physical_bits) - 4096 {
            return Err(VirtualizationError::InvalidRoot);
        }
        // RES1 bit 31, 4-KiB granule, inner-shareable WB read/write-allocate walks.
        let control = (64 - ipa_bits) as u64
            | (sl0 << 6)
            | (1 << 8)
            | (1 << 10)
            | (3 << 12)
            | (ps << 16)
            | (1 << 31);
        Ok(Self { root, control })
    }

    /// Returns the complete VTCR_EL2 value.
    pub const fn control(self) -> u64 {
        self.control
    }

    /// Returns the aligned physical root without a software VM identity.
    pub const fn root(self) -> PhysAddr {
        self.root
    }

    /// Encodes a hardware VMID with this root. The owner must constrain the
    /// VMID to the configured CPU VMID width and perform reuse invalidation.
    pub const fn table_base(self, vmid: u16) -> u64 {
        self.root.as_usize() as u64 | ((vmid as u64) << 48)
    }
}
