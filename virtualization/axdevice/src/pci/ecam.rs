//! Generic ECAM and MMIO-aperture frontends for one PCI root.

use alloc::{boxed::Box, sync::Arc};

use axdevice_base::{
    BusKind, Device, DeviceAccess, DeviceContext, DeviceError, DeviceResult, Resource,
};

use super::{ConfigOffset, PciBdf, PciRootBinding, PciRootState, PciSegment, all_ones};
use crate::{DeviceLifecycle, DeviceManagerResult};

/// Size of one segment-0, bus-0 ECAM window.
pub const PCI_BUS_ZERO_ECAM_SIZE: u64 = 1 << 20;

/// Segment-0, bus-0 ECAM frontend backed by a shared PCI root.
pub struct PciEcamFrontend {
    base: u64,
    root: Arc<PciRootState>,
    resources: Box<[Resource]>,
}

impl PciEcamFrontend {
    /// Creates an ECAM frontend for one graph-resolved 1 MiB window.
    pub fn new(base: u64, root: Arc<PciRootState>) -> Self {
        Self {
            base,
            root,
            resources: alloc::vec![Resource::MmioRange {
                base,
                size: PCI_BUS_ZERO_ECAM_SIZE,
            }]
            .into_boxed_slice(),
        }
    }

    fn selection(&self, access: &DeviceAccess) -> DeviceResult<(PciBdf, ConfigOffset)> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let relative = access
            .address()
            .checked_sub(self.base)
            .filter(|relative| {
                relative
                    .checked_add(access.width().size() as u64)
                    .is_some_and(|end| end <= PCI_BUS_ZERO_ECAM_SIZE)
            })
            .ok_or(DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        let bdf = PciBdf::new(
            PciSegment::new(0),
            (relative >> 20) as u8,
            ((relative >> 15) & 0x1f) as u8,
            ((relative >> 12) & 0x7) as u8,
        )
        .map_err(pci_access_error)?;
        let offset = ConfigOffset::new((relative & 0xfff) as u16).map_err(pci_access_error)?;
        Ok((bdf, offset))
    }
}

impl Device for PciEcamFrontend {
    fn name(&self) -> &str {
        "pci-ecam"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        let (bdf, offset) = self.selection(access)?;
        self.root
            .read_config(bdf, offset, access.width())
            .map_err(pci_access_error)
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        let (bdf, offset) = self.selection(access)?;
        self.root
            .write_config(bdf, offset, access.width(), value)
            .map_err(pci_access_error)
    }
}

/// Single top-level MMIO device owning a PCI root's complete memory aperture.
pub struct PciMmioApertureDevice {
    binding: Arc<PciRootBinding>,
    resources: Box<[Resource]>,
}

impl PciMmioApertureDevice {
    /// Creates an aperture frontend from the graph-resolved range.
    pub fn new(base: u64, size: u64, binding: Arc<PciRootBinding>) -> Self {
        Self {
            binding,
            resources: alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice(),
        }
    }
}

impl Device for PciMmioApertureDevice {
    fn name(&self) -> &str {
        "pci-memory-aperture"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, _context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        match self.binding.read_bar(access.address(), access.width()) {
            Err(DeviceError::NotFound) => Ok(all_ones(access.width().size())),
            result => result,
        }
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        _context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        match self
            .binding
            .write_bar(access.address(), access.width(), value)
        {
            Err(DeviceError::NotFound) => Ok(()),
            result => result,
        }
    }
}

/// Lifecycle adapter restoring root and bound endpoint state.
pub struct PciRootStateLifecycle(Arc<PciRootBinding>);

impl PciRootStateLifecycle {
    /// Creates a lifecycle adapter for one shared root binding.
    pub const fn new(binding: Arc<PciRootBinding>) -> Self {
        Self(binding)
    }
}

impl DeviceLifecycle for PciRootStateLifecycle {
    fn reset(&self) -> DeviceManagerResult {
        self.0.reset()
    }

    fn suspend(&self) -> DeviceManagerResult {
        Ok(())
    }

    fn resume(&self) -> DeviceManagerResult {
        Ok(())
    }
}

fn pci_access_error(error: super::PciError) -> DeviceError {
    DeviceError::InvalidInput {
        operation: "access PCI ECAM",
        detail: alloc::format!("{error}"),
    }
}
