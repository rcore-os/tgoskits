//! Typed PCI function, BAR, and conventional config-space addresses.

use core::fmt;

use super::{PciError, PciResult};
use crate::AccessWidth;

pub(crate) const CONFIG_SPACE_SIZE: usize = 0x100;
const MAX_DEVICE: u8 = 31;
const MAX_FUNCTION: u8 = 7;

/// One PCI segment number.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct PciSegment(u16);

impl PciSegment {
    /// Creates a segment number.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the numeric segment.
    pub const fn value(self) -> u16 {
        self.0
    }
}

/// A validated PCI segment:bus:device.function address.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PciBdf {
    segment: PciSegment,
    bus: u8,
    device: u8,
    function: u8,
}

impl PciBdf {
    /// Creates a BDF after validating the device and function fields.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidAddress`] when `device >= 32` or
    /// `function >= 8`.
    pub fn new(segment: PciSegment, bus: u8, device: u8, function: u8) -> PciResult<Self> {
        if device > MAX_DEVICE {
            return Err(PciError::InvalidAddress {
                component: "device",
                value: u64::from(device),
            });
        }
        if function > MAX_FUNCTION {
            return Err(PciError::InvalidAddress {
                component: "function",
                value: u64::from(function),
            });
        }
        Ok(Self {
            segment,
            bus,
            device,
            function,
        })
    }

    /// Returns the segment.
    pub const fn segment(self) -> PciSegment {
        self.segment
    }

    /// Returns the bus number.
    pub const fn bus(self) -> u8 {
        self.bus
    }

    /// Returns the device number.
    pub const fn device(self) -> u8 {
        self.device
    }

    /// Returns the function number.
    pub const fn function(self) -> u8 {
        self.function
    }

    pub(crate) const fn bus_zero(device: u8) -> Self {
        Self {
            segment: PciSegment::new(0),
            bus: 0,
            device,
            function: 0,
        }
    }
}

impl fmt::Display for PciBdf {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment.value(),
            self.bus,
            self.device,
            self.function
        )
    }
}

/// A validated Type-0 BAR index.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PciBarIndex(u8);

impl PciBarIndex {
    /// Creates a BAR index in `0..=5`.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidAddress`] for an index above BAR5.
    pub fn new(value: u8) -> PciResult<Self> {
        if value >= 6 {
            return Err(PciError::InvalidAddress {
                component: "BAR index",
                value: u64::from(value),
            });
        }
        Ok(Self(value))
    }

    /// Returns the numeric BAR index.
    pub const fn value(self) -> u8 {
        self.0
    }

    pub(crate) const fn config_offset(self) -> usize {
        0x10 + self.0 as usize * 4
    }
}

impl fmt::Display for PciBarIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// A byte offset in one 256-byte conventional PCI config image.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConfigOffset(u16);

impl ConfigOffset {
    /// Creates a conventional config-space offset below `0x100`.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidAddress`] for an extended-config offset.
    pub fn new(value: u16) -> PciResult<Self> {
        if usize::from(value) >= CONFIG_SPACE_SIZE {
            return Err(PciError::InvalidAddress {
                component: "config offset",
                value: u64::from(value),
            });
        }
        Ok(Self(value))
    }

    /// Returns the numeric byte offset.
    pub const fn value(self) -> u16 {
        self.0
    }

    pub(crate) fn validate_access(self, width: AccessWidth) -> PciResult<(usize, usize)> {
        let size = match width {
            AccessWidth::Byte | AccessWidth::Word | AccessWidth::Dword => width.size(),
            AccessWidth::Qword => {
                return Err(PciError::InvalidConfigAccess {
                    offset: self.0,
                    width,
                    detail: "conventional config accesses are limited to 32 bits",
                });
            }
        };
        let offset = usize::from(self.0);
        if offset % size != 0 {
            return Err(PciError::InvalidConfigAccess {
                offset: self.0,
                width,
                detail: "access is not naturally aligned",
            });
        }
        let end = offset
            .checked_add(size)
            .ok_or(PciError::InvalidConfigAccess {
                offset: self.0,
                width,
                detail: "access range overflows",
            })?;
        if end > CONFIG_SPACE_SIZE || offset / 4 != (end - 1) / 4 {
            return Err(PciError::InvalidConfigAccess {
                offset: self.0,
                width,
                detail: "access crosses a config DWORD or function boundary",
            });
        }
        Ok((offset, size))
    }
}
