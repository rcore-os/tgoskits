//! Device-agnostic stage-2 direct-mapping values and the VM update port.
//!
//! Device semantics layers declare *what* they need mapped (GPA ranges with
//! stage-2 permissions); the VM layer implements *how* the nested page table
//! is updated. [`Stage2Remap`] is the only handle crossing that boundary,
//! and [`DirectMapping`] is the only currency it trades in.

use alloc::{format, string::String};
use core::fmt;

use axdevice_base::DeviceError;

use crate::graph::DeviceNodeId;

/// One GPA→HPA direct mapping with stage-2 permissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectMapping {
    gpa_base: u64,
    hpa_base: u64,
    size: u64,
    writable: bool,
    label: &'static str,
}

impl DirectMapping {
    /// Creates one page-aligned mapping.
    ///
    /// # Errors
    ///
    /// Returns `DeviceError::InvalidInput` when `gpa_base`, `hpa_base` or
    /// `size` is not 4 KiB aligned, or `size` is zero.
    pub fn new(
        gpa_base: u64,
        hpa_base: u64,
        size: u64,
        writable: bool,
        label: &'static str,
    ) -> Result<Self, DeviceError> {
        const PAGE: u64 = 0x1000;
        let reject = |detail: String| DeviceError::InvalidInput {
            operation: "create direct mapping",
            detail,
        };
        if size == 0 {
            return Err(reject("mapping size is zero".into()));
        }
        if !gpa_base.is_multiple_of(PAGE)
            || !hpa_base.is_multiple_of(PAGE)
            || !size.is_multiple_of(PAGE)
        {
            return Err(reject(format!(
                "mapping gpa {gpa_base:#x}, hpa {hpa_base:#x} and size {size:#x} must all be \
                 {PAGE:#x}-aligned"
            )));
        }
        Ok(Self {
            gpa_base,
            hpa_base,
            size,
            writable,
            label,
        })
    }

    /// Returns the guest-physical base of the mapping.
    pub const fn gpa_base(self) -> u64 {
        self.gpa_base
    }

    /// Returns the host-physical base of the mapping.
    pub const fn hpa_base(self) -> u64 {
        self.hpa_base
    }

    /// Returns the mapping size in bytes.
    pub const fn size(self) -> u64 {
        self.size
    }

    /// Returns whether the guest may write the mapped range.
    pub const fn writable(self) -> bool {
        self.writable
    }

    /// Returns the stable section name used in fault diagnostics.
    pub const fn label(self) -> &'static str {
        self.label
    }
}

/// One guest-physical range to revoke, identified by its current mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GpaRange {
    base: u64,
    size: u64,
}

impl GpaRange {
    /// Creates one range over `base..base + size`.
    pub const fn new(base: u64, size: u64) -> Self {
        Self { base, size }
    }

    /// Returns the range base.
    pub const fn base(self) -> u64 {
        self.base
    }

    /// Returns the range size in bytes.
    pub const fn size(self) -> u64 {
        self.size
    }
}

/// Diagnosis of one faulting GPA against committed mappings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectMappingFault {
    owner: DeviceNodeId,
    label: &'static str,
    writable: bool,
}

impl DirectMappingFault {
    /// Assembles one fault diagnosis. `owner` is stored by value; node IDs
    /// clone cheaply.
    pub fn new(owner: DeviceNodeId, label: &'static str, writable: bool) -> Self {
        Self {
            owner,
            label,
            writable,
        }
    }

    /// Returns the device node that owns the mapped range.
    pub fn owner(&self) -> &DeviceNodeId {
        &self.owner
    }

    /// Returns the stable section name of the mapped range.
    pub const fn label(&self) -> &'static str {
        self.label
    }

    /// Returns whether the mapping would have allowed the write.
    pub const fn writable(&self) -> bool {
        self.writable
    }
}

/// VM-owned stage-2 update port, published once per VM and handed to device
/// builds that declare direct mappings.
///
/// Implementations live in the VM layer because they touch nested page
/// tables; devices only express mapping intent through this port.
pub trait Stage2Remap: Send + Sync {
    /// Replaces the direct mappings owned by `owner`.
    ///
    /// Implementations must install every `commit` entry before revoking
    /// `revoke`, so a same-address rewrite never exposes a gap. Commit
    /// entries must not overlap ranges owned by other devices.
    ///
    /// # Errors
    ///
    /// Returns `DeviceError::Backend` when the stage-2 update fails; the
    /// caller keeps its previous mapping state in that case. Owner conflicts
    /// with other devices' committed ranges return `DeviceError::ResourceBusy`.
    fn update(
        &self,
        owner: &DeviceNodeId,
        revoke: &[GpaRange],
        commit: &[DirectMapping],
    ) -> Result<(), DeviceError>;

    /// Diagnoses one faulting GPA against currently committed mappings.
    /// Returns `None` when the GPA belongs to no direct mapping.
    fn diagnose(&self, gpa: u64) -> Option<DirectMappingFault>;
}

impl fmt::Debug for dyn Stage2Remap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Stage2Remap")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(name: &str) -> DeviceNodeId {
        DeviceNodeId::new(name).unwrap()
    }

    #[test]
    fn accepts_page_aligned_mappings() {
        let mapping = DirectMapping::new(0x1000, 0x5000, 0x2000, true, "output section").unwrap();
        assert_eq!(mapping.gpa_base(), 0x1000);
        assert_eq!(mapping.hpa_base(), 0x5000);
        assert_eq!(mapping.size(), 0x2000);
        assert!(mapping.writable());
        assert_eq!(mapping.label(), "output section");
    }

    #[test]
    fn rejects_misaligned_or_empty_mappings() {
        assert!(DirectMapping::new(0x800, 0x1000, 0x1000, true, "x").is_err());
        assert!(DirectMapping::new(0x1000, 0x1800, 0x1000, false, "x").is_err());
        assert!(DirectMapping::new(0x1000, 0x1000, 0x800, true, "x").is_err());
        assert!(DirectMapping::new(0x1000, 0x1000, 0, true, "x").is_err());
    }

    #[test]
    fn gpa_ranges_expose_their_bounds() {
        let range = GpaRange::new(0x2000, 0x3000);
        assert_eq!(range.base(), 0x2000);
        assert_eq!(range.size(), 0x3000);
    }

    #[test]
    fn fault_diagnoses_carry_owner_and_permission() {
        let fault = DirectMappingFault::new(node("ivshmem0"), "state table", false);
        assert_eq!(*fault.owner(), node("ivshmem0"));
        assert_eq!(fault.label(), "state table");
        assert!(!fault.writable());
    }
}
