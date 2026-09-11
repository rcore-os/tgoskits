//! Architecture-neutral PCI configuration and memory-aperture frontends.

use alloc::{boxed::Box, sync::Arc};

use axdevice_base::*;

use super::{ConfigOffset, PciBdf, PciRootBinding, PciSegment, all_ones};
use crate::{DeviceLifecycle, DeviceManagerResult};

/// MMIO ECAM frontend for one conventional PCI root.
pub struct PciEcamConfigFrontend {
    base: u64,
    size: u64,
    binding: Arc<PciRootBinding>,
    resources: Box<[Resource]>,
}

impl PciEcamConfigFrontend {
    /// Creates an ECAM frontend over the graph-resolved bus window.
    pub fn new(base: u64, size: u64, binding: Arc<PciRootBinding>) -> Self {
        Self {
            base,
            size,
            binding,
            resources: alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice(),
        }
    }

    fn selection(&self, access: &DeviceAccess) -> DeviceResult<Option<(PciBdf, ConfigOffset)>> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        let offset = access
            .address()
            .checked_sub(self.base)
            .filter(|offset| *offset < self.size)
            .ok_or(DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        offset
            .checked_add(access.width().size() as u64)
            .filter(|end| *end <= self.size)
            .ok_or(DeviceError::OutOfRange {
                addr: access.address(),
            })?;
        let function_offset = (offset & 0xfff) as u16;
        if function_offset >= 0x100 {
            return Ok(None);
        }
        let bdf = PciBdf::new(
            PciSegment::new(0),
            (offset >> 20) as u8,
            ((offset >> 15) & 0x1f) as u8,
            ((offset >> 12) & 0x7) as u8,
        )
        .map_err(pci_access_error)?;
        let register = ConfigOffset::new(function_offset).map_err(pci_access_error)?;
        Ok(Some((bdf, register)))
    }
}

impl Device for PciEcamConfigFrontend {
    fn name(&self) -> &str {
        "pci-ecam-config"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        match self.selection(access)? {
            Some((bdf, register)) => {
                self.binding
                    .read_config_with_context(bdf, register, access.width(), context)
            }
            None => Ok(all_ones(access.width().size())),
        }
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        if let Some((bdf, register)) = self.selection(access)? {
            self.binding.write_config_with_context(
                bdf,
                register,
                access.width(),
                value,
                context,
            )?;
        }
        Ok(())
    }
}

/// Single top-level MMIO device owning a PCI root's complete memory aperture.
pub struct PciMemoryApertureDevice {
    binding: Arc<PciRootBinding>,
    resources: Box<[Resource]>,
}

impl PciMemoryApertureDevice {
    /// Creates the aperture adapter from the graph-resolved range.
    pub fn new(base: u64, size: u64, binding: Arc<PciRootBinding>) -> Self {
        Self {
            binding,
            resources: alloc::vec![Resource::MmioRange { base, size }].into_boxed_slice(),
        }
    }
}

impl Device for PciMemoryApertureDevice {
    fn name(&self) -> &str {
        "pci-memory-aperture"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn read(&self, access: &DeviceAccess, context: &mut dyn DeviceContext) -> DeviceResult<u64> {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        match self
            .binding
            .read_bar_with_context(access.address(), access.width(), context)
        {
            Err(DeviceError::NotFound) => Ok(all_ones(access.width().size())),
            result => result,
        }
    }

    fn write(
        &self,
        access: &DeviceAccess,
        value: u64,
        context: &mut dyn DeviceContext,
    ) -> DeviceResult {
        if access.bus() != BusKind::Mmio {
            return Err(DeviceError::OutOfRange {
                addr: access.address(),
            });
        }
        match self
            .binding
            .write_bar_with_context(access.address(), access.width(), value, context)
        {
            Err(DeviceError::NotFound) => Ok(()),
            result => result,
        }
    }
}

/// Lifecycle adapter restoring the PCI root and all bound endpoint state.
pub struct PciRootLifecycle(Arc<PciRootBinding>);

impl PciRootLifecycle {
    /// Creates a lifecycle adapter for one generic PCI root.
    pub const fn new(binding: Arc<PciRootBinding>) -> Self {
        Self(binding)
    }
}

impl DeviceLifecycle for PciRootLifecycle {
    fn reset(&self) -> DeviceManagerResult {
        self.0.reset_lifecycle()
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
        operation: "access PCI ECAM configuration",
        detail: alloc::format!("{error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        DeviceNodeId, PciClass, PciEndpointIdentity, PciFunctionSpec, PciRootState,
        PciTopologyBuilder, ResourceRequest,
    };

    const ECAM_BASE: u64 = 0x2000_0000;
    const ECAM_SIZE: u64 = 0x0800_0000;

    fn frontend() -> PciEcamConfigFrontend {
        let mut topology = PciTopologyBuilder::new();
        topology
            .add_function(
                PciFunctionSpec::new(
                    DeviceNodeId::new("endpoint").unwrap(),
                    PciEndpointIdentity::new(0x1af4, 0x1042, PciClass::new(1, 0x80, 0)),
                )
                .with_bdf(ResourceRequest::Fixed(
                    PciBdf::new(PciSegment::new(0), 0, 3, 0).unwrap(),
                )),
            )
            .unwrap();
        let topology = Arc::new(topology.resolve(0x4000_0000..0x8000_0000).unwrap());
        let root = Arc::new(PciRootState::new(topology));
        let binding = Arc::new(PciRootBinding::new(
            DeviceNodeId::new("pci-host").unwrap(),
            root,
        ));
        PciEcamConfigFrontend::new(ECAM_BASE, ECAM_SIZE, binding)
    }

    fn access(address: u64, width: AccessWidth) -> DeviceAccess {
        DeviceAccess::new(DeviceVcpuId::new(0), BusKind::Mmio, address, width)
    }

    #[test]
    fn ecam_decodes_bdf_and_treats_extended_space_as_unimplemented() {
        let frontend = frontend();
        let mut context = NoopDeviceContext::new(DeviceId::new(0));
        assert_eq!(
            frontend
                .read(
                    &access(ECAM_BASE + (3 << 15), AccessWidth::Dword),
                    &mut context
                )
                .unwrap(),
            0x1042_1af4
        );
        assert_eq!(
            frontend
                .read(
                    &access(ECAM_BASE + (3 << 15) + 0x100, AccessWidth::Dword),
                    &mut context,
                )
                .unwrap(),
            u32::MAX as u64
        );
        assert_eq!(
            frontend
                .read(
                    &access(ECAM_BASE + (4 << 15), AccessWidth::Word),
                    &mut context
                )
                .unwrap(),
            u16::MAX as u64
        );
    }
}
