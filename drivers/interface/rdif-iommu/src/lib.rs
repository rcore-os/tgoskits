#![no_std]

extern crate alloc;

use alloc::sync::Arc;
use core::{fmt, ops::BitOr};

use rdif_base::DriverGeneric;

/// Hardware stream identifier supplied by the PCI IOMMU map.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct StreamId(pub u32);

/// Unique identity of a translation domain, not a device address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DmaDomainId(pub u64);

/// Half-open IOVA aperture. Address zero remains reserved for invalid DMA handles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IovaWindow {
    pub start: u64,
    pub end: u64,
}

impl IovaWindow {
    pub const fn contains(self, start: u64, len: usize) -> bool {
        match start.checked_add(len as u64) {
            Some(end) => start >= self.start && end <= self.end,
            None => false,
        }
    }
}

/// Access and memory type requested by a DMA mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MapPermissions(u8);

impl MapPermissions {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    /// The physical target is an MMIO page, such as an MSI doorbell.
    pub const MMIO: Self = Self(4);

    pub const fn can_read(self) -> bool {
        self.0 & Self::READ.0 != 0
    }

    pub const fn can_write(self) -> bool {
        self.0 & Self::WRITE.0 != 0
    }

    pub const fn is_mmio(self) -> bool {
        self.0 & Self::MMIO.0 != 0
    }
}

impl BitOr for MapPermissions {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IommuError {
    UnsupportedHardware,
    InvalidAddress,
    InvalidLength,
    InvalidPermissions,
    OutOfMemory,
    AlreadyBound,
    AlreadyMapped,
    NotMapped,
    NoIdentifiers,
    Busy,
    CommandTimeout,
    CommandError(u32),
    QueueOverflow,
}

impl fmt::Display for IommuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IOMMU error: {self:?}")
    }
}

impl core::error::Error for IommuError {}

/// A bound device owns this domain for the lifetime of its DMA resources.
pub trait IommuDomain: Send + Sync {
    fn id(&self) -> DmaDomainId;
    fn window(&self) -> IovaWindow;

    /// Map a physically contiguous, page-aligned range of 4 KiB pages.
    ///
    /// A single-page call must leave that page unmapped on error. Callers
    /// using larger ranges must account for partial publication if rollback
    /// or its invalidation fails.
    fn map_pages(
        &self,
        iova: u64,
        physical: u64,
        len: usize,
        permissions: MapPermissions,
    ) -> Result<(), IommuError>;

    /// Unmap and complete the IOTLB invalidation before returning success.
    /// A failed call requires the caller to quarantine the IOVA and backing pages.
    fn unmap_and_sync(&self, iova: u64, len: usize) -> Result<(), IommuError>;
}

pub trait IommuController: Send + Sync {
    /// Duplicate StreamIds are rejected; domains are not hot-swappable.
    fn bind(&self, stream: StreamId) -> Result<Arc<dyn IommuDomain>, IommuError>;
}

/// Registry-facing provider. The registry owns the controller while domains
/// retain their own reference to the hardware state.
pub struct Iommu {
    name: &'static str,
    inner: Arc<dyn IommuController>,
}

impl Iommu {
    pub fn new(name: &'static str, controller: Arc<dyn IommuController>) -> Self {
        Self {
            name,
            inner: controller,
        }
    }

    pub fn bind(&self, stream: StreamId) -> Result<Arc<dyn IommuDomain>, IommuError> {
        self.inner.bind(stream)
    }
}

impl DriverGeneric for Iommu {
    fn name(&self) -> &str {
        self.name
    }
}
