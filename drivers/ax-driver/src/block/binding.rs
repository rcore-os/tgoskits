use alloc::{
    boxed::Box,
    string::{String, ToString},
    vec::Vec,
};

use ax_errno::AxError;
use log::warn;
use rdif_block::{BlockController, BlockControllerGroup};
use rdrive::{Device, probe::OnProbeError};

use crate::{
    BindingInfo, BindingIrq, IrqBindingLease, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

/// Registered platform object that still owns its portable block controller.
pub struct PlatformBlockDevice {
    name: String,
    controller: Option<Box<dyn BlockController>>,
    info: BindingInfo,
}

/// Registered platform object that owns one shared multi-device controller.
pub struct PlatformBlockGroup {
    name: String,
    controller: Option<Box<dyn BlockControllerGroup>>,
    info: BindingInfo,
}

/// A controller removed from `rdrive` and ready for the block runtime.
pub struct RdifBlockDevice {
    name: String,
    irqs: Vec<crate::BindingIrqBinding>,
    controller: Box<dyn BlockController>,
}

/// A controller group removed from `rdrive` and ready for the block runtime.
pub struct RdifBlockGroup {
    name: String,
    irqs: Vec<crate::BindingIrqBinding>,
    controller: Box<dyn BlockControllerGroup>,
}

impl PlatformBlockDevice {
    fn new(name: String, controller: Box<dyn BlockController>, info: BindingInfo) -> Self {
        Self {
            name,
            controller: Some(controller),
            info,
        }
    }
}

impl PlatformBlockGroup {
    fn new(name: String, controller: Box<dyn BlockControllerGroup>, info: BindingInfo) -> Self {
        Self {
            name,
            controller: Some(controller),
            info,
        }
    }
}

impl rdrive::DriverGeneric for PlatformBlockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl rdrive::DriverGeneric for PlatformBlockGroup {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformBlockDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

impl BoundDevice for PlatformBlockGroup {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

impl RdifBlockDevice {
    /// Returns the stable registered device name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the first binding IRQ, if any.
    pub fn irq(&self) -> Option<&BindingIrq> {
        self.irqs.first().map(|binding| &binding.irq)
    }

    /// Returns one binding IRQ by controller-local source identifier.
    pub fn irq_for_source(&self, source_id: usize) -> Option<&BindingIrq> {
        self.irqs
            .iter()
            .find(|binding| binding.source_id == source_id)
            .map(|binding| &binding.irq)
    }

    /// Returns every platform IRQ binding for this controller.
    pub fn irq_sources(&self) -> &[crate::BindingIrqBinding] {
        &self.irqs
    }

    /// Transfers the portable controller to the filesystem block runtime.
    pub fn into_controller(self) -> Box<dyn BlockController> {
        self.controller
    }

    /// Splits platform metadata from the portable controller.
    pub fn into_parts(
        self,
    ) -> (
        String,
        Vec<crate::BindingIrqBinding>,
        Box<dyn BlockController>,
    ) {
        (self.name, self.irqs, self.controller)
    }
}

impl TryFrom<Device<PlatformBlockDevice>> for RdifBlockDevice {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockDevice>) -> Result<Self, Self::Error> {
        let mut device = base.lock().map_err(|_| AxError::BadState)?;
        let name = device.name.clone();
        let irqs = device.info.irq_sources().to_vec();
        let controller = device.controller.take().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            irqs,
            controller,
        })
    }
}

impl RdifBlockGroup {
    /// Splits platform metadata from the portable controller group.
    pub fn into_parts(
        self,
    ) -> (
        String,
        Vec<crate::BindingIrqBinding>,
        Box<dyn BlockControllerGroup>,
    ) {
        (self.name, self.irqs, self.controller)
    }
}

impl TryFrom<Device<PlatformBlockGroup>> for RdifBlockGroup {
    type Error = AxError;

    fn try_from(base: Device<PlatformBlockGroup>) -> Result<Self, Self::Error> {
        let mut group = base.lock().map_err(|_| AxError::BadState)?;
        let name = group.name.clone();
        let irqs = group.info.irq_sources().to_vec();
        let controller = group.controller.take().ok_or(AxError::BadState)?;
        Ok(Self {
            name,
            irqs,
            controller,
        })
    }
}

/// Registers a portable block controller discovered by a platform probe.
pub trait PlatformDeviceBlock {
    /// Registers a controller without platform metadata.
    fn register_block<T: BlockController>(self, controller: T) -> Option<usize>;

    /// Registers a controller with resolved platform IRQ metadata.
    fn register_block_with_info<T: BlockController>(
        self,
        controller: T,
        info: BindingInfo,
    ) -> Option<usize>;

    /// Registers a controller wrapped by an owned IRQ-binding lease.
    fn register_irq_bound_block<T, L>(self, controller: T, irq_lease: L) -> Option<usize>
    where
        Self: Sized,
        T: BlockController,
        L: IrqBindingLease;
}

impl PlatformDeviceBlock for rdrive::PlatformDevice {
    fn register_block<T: BlockController>(self, controller: T) -> Option<usize> {
        self.register_block_with_info(controller, BindingInfo::empty())
    }

    fn register_block_with_info<T: BlockController>(
        self,
        controller: T,
        info: BindingInfo,
    ) -> Option<usize> {
        register_block_with_info(self, controller, info)
    }

    fn register_irq_bound_block<T, L>(self, controller: T, irq_lease: L) -> Option<usize>
    where
        T: BlockController,
        L: IrqBindingLease,
    {
        let info = irq_lease.binding_info();
        self.register_block_with_info(super::IrqBoundBlock::new(controller, irq_lease), info)
    }
}

/// Registers a portable multi-device block controller discovered by a probe.
pub trait PlatformDeviceBlockGroup {
    /// Registers a controller group with resolved platform IRQ metadata.
    fn register_block_group_with_info<T: BlockControllerGroup>(
        self,
        controller: T,
        info: BindingInfo,
    ) -> Option<usize>;
}

impl PlatformDeviceBlockGroup for rdrive::PlatformDevice {
    fn register_block_group_with_info<T: BlockControllerGroup>(
        self,
        controller: T,
        info: BindingInfo,
    ) -> Option<usize> {
        register_block_group_with_info(self, controller, info)
    }
}

/// Registers a portable block controller from an FDT probe.
pub trait ProbeFdtBlock {
    /// Resolves FDT bindings and registers the controller.
    fn register_block<T: BlockController>(
        self,
        controller: T,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeFdtBlock for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_block<T: BlockController>(
        self,
        controller: T,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            controller,
            info,
        ))
    }
}

/// Registers a portable multi-device block controller from an FDT probe.
pub trait ProbeFdtBlockGroup {
    /// Resolves FDT bindings and registers the controller group.
    fn register_block_group<T: BlockControllerGroup>(
        self,
        controller: T,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeFdtBlockGroup for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_block_group<T: BlockControllerGroup>(
        self,
        controller: T,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_block_group_with_info(
            self.into_platform_device(),
            controller,
            info,
        ))
    }
}

/// Registers a portable block controller from an ACPI probe.
pub trait ProbeAcpiBlock {
    /// Resolves ACPI bindings and registers the controller.
    fn register_block<T: BlockController>(
        self,
        controller: T,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeAcpiBlock for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_block<T: BlockController>(
        self,
        controller: T,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_acpi(self.info())?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            controller,
            info,
        ))
    }
}

/// Registers a portable block controller from a PCI probe.
#[cfg(feature = "pci")]
pub trait ProbePciBlock {
    /// Resolves the requested PCI IRQ binding and registers the controller.
    fn register_block<T: BlockController>(
        self,
        controller: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>;
}

#[cfg(feature = "pci")]
impl ProbePciBlock for rdrive::probe::pci::ProbePci<'_> {
    fn register_block<T: BlockController>(
        self,
        controller: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_block_with_info(
            self.into_platform_device(),
            controller,
            info,
        ))
    }
}

/// Registers a portable multi-device block controller from a PCI probe.
#[cfg(feature = "pci")]
pub trait ProbePciBlockGroup {
    /// Resolves the requested PCI IRQ and registers the controller group.
    fn register_block_group<T: BlockControllerGroup>(
        self,
        controller: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>;
}

#[cfg(feature = "pci")]
impl ProbePciBlockGroup for rdrive::probe::pci::ProbePci<'_> {
    fn register_block_group<T: BlockControllerGroup>(
        self,
        controller: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_block_group_with_info(
            self.into_platform_device(),
            controller,
            info,
        ))
    }
}

fn register_block_with_info<T: BlockController>(
    platform: rdrive::PlatformDevice,
    controller: T,
    info: BindingInfo,
) -> Option<usize> {
    let name = controller.name().to_string();
    register_bound_device(
        platform,
        PlatformBlockDevice::new(name, Box::new(controller), info),
    )
}

fn register_block_group_with_info<T: BlockControllerGroup>(
    platform: rdrive::PlatformDevice,
    controller: T,
    info: BindingInfo,
) -> Option<usize> {
    let name = controller.name().to_string();
    register_bound_device(
        platform,
        PlatformBlockGroup::new(name, Box::new(controller), info),
    )
}

/// Removes every registered block controller from `rdrive`.
pub fn take_rdif_block_devices() -> Vec<RdifBlockDevice> {
    rdrive::get_list::<PlatformBlockDevice>()
        .into_iter()
        .filter_map(|device| match RdifBlockDevice::try_from(device) {
            Ok(block) => Some(block),
            Err(error) => {
                warn!("failed to take block controller: {error:?}");
                None
            }
        })
        .collect()
}

/// Removes every registered multi-device block controller from `rdrive`.
pub fn take_rdif_block_groups() -> Vec<RdifBlockGroup> {
    rdrive::get_list::<PlatformBlockGroup>()
        .into_iter()
        .filter_map(|device| match RdifBlockGroup::try_from(device) {
            Ok(group) => Some(group),
            Err(error) => {
                warn!("failed to take block controller group: {error:?}");
                None
            }
        })
        .collect()
}

/// Maps a portable block error at the ArceOS integration boundary.
pub fn map_blk_err_to_ax_err(error: rdif_block::BlkError) -> AxError {
    match error {
        rdif_block::BlkError::NotSupported => AxError::Unsupported,
        rdif_block::BlkError::Retry => AxError::WouldBlock,
        rdif_block::BlkError::NoMemory => AxError::NoMemory,
        rdif_block::BlkError::InvalidBlockIndex(_) | rdif_block::BlkError::InvalidRequest => {
            AxError::InvalidInput
        }
        rdif_block::BlkError::TimedOut => AxError::TimedOut,
        rdif_block::BlkError::Io | rdif_block::BlkError::Other(_) => AxError::Io,
    }
}
