use core::fmt::{Display, Formatter};

use crate::{VgicError, VgicResult};

/// First architectural shared peripheral interrupt identifier.
pub const ARM_SPI_INTID_MIN: u32 = 32;
/// Last non-special traditional interrupt identifier.
pub const ARM_SPI_INTID_MAX: u32 = 1019;

/// A checked architectural SPI INTID.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ArmSpiIntId(u32);

impl ArmSpiIntId {
    /// Creates an SPI INTID in the inclusive range 32..=1019.
    pub const fn new(value: u32) -> VgicResult<Self> {
        if value >= ARM_SPI_INTID_MIN && value <= ARM_SPI_INTID_MAX {
            Ok(Self(value))
        } else {
            Err(VgicError::InvalidSpiIntId {
                value: value as usize,
            })
        }
    }

    /// Returns the architectural INTID.
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl Display for ArmSpiIntId {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl TryFrom<usize> for ArmSpiIntId {
    type Error = VgicError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        let value_u32 = u32::try_from(value).map_err(|_| VgicError::InvalidSpiIntId { value })?;
        Self::new(value_u32).map_err(|_| VgicError::InvalidSpiIntId { value })
    }
}

/// A vCPU identifier used by the VGIC state machine.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct VgicVcpuId(u32);

impl VgicVcpuId {
    /// Creates an identifier from its lossless representation.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the numeric vCPU identifier.
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl TryFrom<usize> for VgicVcpuId {
    type Error = VgicError;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        u32::try_from(value)
            .map(Self)
            .map_err(|_| VgicError::InvalidVcpuId { value })
    }
}

/// Unique identity of one installed delivery instance.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeliveryEpoch(u64);

impl DeliveryEpoch {
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the monotonically allocated epoch value.
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}
