//! Validated GICv3 identifiers and value types.

use alloc::vec::Vec;

use crate::{VgicError, VgicResult};

/// First architectural LPI INTID.
pub const LPI_INTID_BASE: u32 = 8192;
/// Highest INTID representable by GICv3 LPIs.
pub const LPI_INTID_MAX: u32 = 0x00ff_ffff;

macro_rules! bounded_id {
    ($name:ident, $inner:ty, $start:expr, $end:expr, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
        #[repr(transparent)]
        pub struct $name($inner);

        impl $name {
            /// Validates and creates an identifier from its architectural value.
            pub fn new(raw: $inner) -> VgicResult<Self> {
                if ($start..$end).contains(&raw) {
                    Ok(Self(raw))
                } else {
                    Err(VgicError::InvalidIntId { raw: raw as u32 })
                }
            }

            /// Returns the architectural numeric value.
            pub const fn raw(self) -> $inner {
                self.0
            }
        }
    };
}

bounded_id!(SgiId, u8, 0, 16, "Software-generated interrupt identifier.");
bounded_id!(
    PpiId,
    u8,
    16,
    32,
    "Private peripheral interrupt identifier."
);
bounded_id!(
    SpiId,
    u32,
    32,
    1020,
    "Shared peripheral interrupt identifier."
);
bounded_id!(
    LpiId,
    u32,
    LPI_INTID_BASE,
    LPI_INTID_MAX + 1,
    "Locality-specific peripheral interrupt identifier."
);

/// A validated GICv3 interrupt identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IntId {
    /// SGI 0-15.
    Sgi(SgiId),
    /// PPI 16-31.
    Ppi(PpiId),
    /// SPI 32-1019.
    Spi(SpiId),
    /// LPI 8192-0x00ff_ffff.
    Lpi(LpiId),
}

impl IntId {
    /// Classifies an architectural INTID and rejects reserved ranges.
    pub fn new(raw: u32) -> VgicResult<Self> {
        match raw {
            0..=15 => Ok(Self::Sgi(SgiId(raw as u8))),
            16..=31 => Ok(Self::Ppi(PpiId(raw as u8))),
            32..=1019 => Ok(Self::Spi(SpiId(raw))),
            LPI_INTID_BASE..=LPI_INTID_MAX => Ok(Self::Lpi(LpiId(raw))),
            _ => Err(VgicError::InvalidIntId { raw }),
        }
    }

    /// Returns the architectural numeric INTID.
    pub const fn raw(self) -> u32 {
        match self {
            Self::Sgi(id) => id.raw() as u32,
            Self::Ppi(id) => id.raw() as u32,
            Self::Spi(id) => id.raw(),
            Self::Lpi(id) => id.raw(),
        }
    }
}

/// VM-local vCPU identifier used by the controller.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct GicVcpuId(usize);

impl GicVcpuId {
    /// Creates a VM-local identifier.
    pub const fn new(raw: usize) -> Self {
        Self(raw)
    }

    /// Returns the VM-local number.
    pub const fn raw(self) -> usize {
        self.0
    }
}

/// Four-level GIC affinity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct GicAffinity {
    aff3: u8,
    aff2: u8,
    aff1: u8,
    aff0: u8,
}

impl GicAffinity {
    /// Creates a GIC affinity from its four fields.
    pub const fn new(aff3: u8, aff2: u8, aff1: u8, aff0: u8) -> Self {
        Self {
            aff3,
            aff2,
            aff1,
            aff0,
        }
    }

    /// Decodes affinity fields from an MPIDR-style value.
    pub const fn from_mpidr(mpidr: u64) -> Self {
        Self::new(
            ((mpidr >> 32) & 0xff) as u8,
            ((mpidr >> 16) & 0xff) as u8,
            ((mpidr >> 8) & 0xff) as u8,
            (mpidr & 0xff) as u8,
        )
    }

    /// Returns an MPIDR-style packed affinity.
    pub const fn mpidr(self) -> u64 {
        ((self.aff3 as u64) << 32)
            | ((self.aff2 as u64) << 16)
            | ((self.aff1 as u64) << 8)
            | self.aff0 as u64
    }

    /// Returns affinity level 3.
    pub const fn aff3(self) -> u8 {
        self.aff3
    }

    /// Returns affinity level 2.
    pub const fn aff2(self) -> u8 {
        self.aff2
    }

    /// Returns affinity level 1.
    pub const fn aff1(self) -> u8 {
        self.aff1
    }

    /// Returns affinity level 0.
    pub const fn aff0(self) -> u8 {
        self.aff0
    }
}

/// Interrupt priority, where a lower numeric value has higher priority.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct Priority(u8);

impl Priority {
    /// Default non-secure interrupt priority.
    pub const DEFAULT: Self = Self(0x80);

    /// Creates a priority value.
    pub const fn new(raw: u8) -> Self {
        Self(raw)
    }

    /// Returns the raw priority byte.
    pub const fn raw(self) -> u8 {
        self.0
    }
}

/// Electrical trigger behavior for a wired interrupt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerMode {
    /// Edge-triggered input.
    Edge,
    /// Level-sensitive input.
    Level,
}

/// GIC interrupt lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InterruptState {
    /// Neither pending nor active.
    Inactive,
    /// Pending delivery.
    Pending,
    /// Acknowledged by a vCPU.
    Active,
    /// Active with another delivery pending.
    ActivePending,
}

/// ITS device identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct ItsDeviceId(u32);

impl ItsDeviceId {
    /// Creates an ITS device identifier.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw identifier.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// ITS event identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct EventId(u32);

impl EventId {
    /// Creates an ITS event identifier.
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    /// Returns the raw identifier.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// ITS collection identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct CollectionId(u16);

impl CollectionId {
    /// Creates an ITS collection identifier.
    pub const fn new(raw: u16) -> Self {
        Self(raw)
    }

    /// Returns the raw identifier.
    pub const fn raw(self) -> u16 {
        self.0
    }
}

/// Target selection for one SGI operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SgiTarget {
    /// Explicit affinity list.
    Affinities(Vec<GicAffinity>),
    /// Every attached vCPU except the sender.
    AllExceptSelf,
    /// Only the sender.
    SelfOnly,
}

/// Platform-owned physical interrupt identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct PhysicalIrqId(u64);

impl PhysicalIrqId {
    /// Creates an identifier from an adapter-defined stable value.
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Returns the adapter-defined value.
    pub const fn raw(self) -> u64 {
        self.0
    }
}
