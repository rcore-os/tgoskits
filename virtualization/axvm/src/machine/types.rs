//! Strong identifiers and address ranges used by VM machine planning.

use alloc::string::String;
use core::fmt::{Display, Formatter};

use super::{MachinePlanError, MachinePlanResult};

/// A non-empty half-open address range `[base, end)`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AddressRange {
    base: u64,
    size: u64,
}

/// A non-empty x86-style port-I/O interval.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IoPortRange {
    base: u16,
    size: u16,
}

impl IoPortRange {
    /// Creates a checked half-open port range.
    pub fn new(base: u16, size: u16) -> MachinePlanResult<Self> {
        if size == 0 || u32::from(base) + u32::from(size) > 0x1_0000 {
            return Err(MachinePlanError::InvalidPortRange { base, size });
        }
        Ok(Self { base, size })
    }

    /// Returns the first port.
    pub const fn base(self) -> u16 {
        self.base
    }

    /// Returns the number of ports.
    pub const fn size(self) -> u16 {
        self.size
    }

    /// Returns the exclusive end as a value capable of representing 0x10000.
    pub const fn end(self) -> u32 {
        self.base as u32 + self.size as u32
    }
}

impl AddressRange {
    /// Creates a checked non-empty address range.
    pub fn new(base: u64, size: u64) -> MachinePlanResult<Self> {
        if size == 0 || base.checked_add(size).is_none() {
            return Err(MachinePlanError::InvalidAddressRange { base, size });
        }
        Ok(Self { base, size })
    }

    /// Returns the first address in the range.
    pub const fn base(self) -> u64 {
        self.base
    }

    /// Returns the range length.
    pub const fn size(self) -> u64 {
        self.size
    }

    /// Returns the exclusive end address.
    pub const fn end(self) -> u64 {
        self.base + self.size
    }

    /// Returns whether the range contains one address.
    pub const fn contains(self, address: u64) -> bool {
        self.base <= address && address < self.end()
    }

    /// Returns whether two ranges overlap.
    pub const fn overlaps(self, other: Self) -> bool {
        self.base < other.end() && other.base < self.end()
    }

    pub(crate) fn intersection(self, other: Self) -> Option<Self> {
        let base = self.base.max(other.base);
        let end = self.end().min(other.end());
        if base < end {
            Some(Self {
                base,
                size: end - base,
            })
        } else {
            None
        }
    }

    pub(crate) fn from_bounds(base: u64, end: u64) -> Option<Self> {
        if base < end {
            Some(Self {
                base,
                size: end - base,
            })
        } else {
            None
        }
    }
}

/// Stable identity of a host firmware device.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct HostDeviceId(String);

impl HostDeviceId {
    /// Creates a checked host device identity.
    pub fn new(value: impl Into<String>) -> MachinePlanResult<Self> {
        checked_identifier("host device", value).map(Self)
    }

    /// Returns the textual stable identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for HostDeviceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stable identity of one virtual device instance.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeviceInstanceId(String);

impl DeviceInstanceId {
    /// Creates a checked virtual device instance identity.
    pub fn new(value: impl Into<String>) -> MachinePlanResult<Self> {
        checked_identifier("virtual device instance", value).map(Self)
    }

    /// Returns the textual instance identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for DeviceInstanceId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn checked_identifier(kind: &'static str, value: impl Into<String>) -> MachinePlanResult<String> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(MachinePlanError::EmptyIdentifier { kind });
    }
    Ok(value)
}

pub(crate) fn selector_label(prefix: &str, value: impl Display) -> String {
    alloc::format!("{prefix}:{value}")
}
