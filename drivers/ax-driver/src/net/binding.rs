extern crate alloc;

use alloc::boxed::Box;

use dma_api::DeviceDma;
use rd_net::{NetDevice, NetError};
use rdrive::{Device, DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

pub struct PlatformNetDevice {
    name: &'static str,
    info: BindingInfo,
    device: Option<Box<dyn NetDevice>>,
    dma: Option<DeviceDma>,
}

impl PlatformNetDevice {
    fn new(
        name: &'static str,
        device: Box<dyn NetDevice>,
        dma: DeviceDma,
        info: BindingInfo,
    ) -> Self {
        Self {
            name,
            info,
            device: Some(device),
            dma: Some(dma),
        }
    }

    fn take_net(&mut self) -> Option<TakenNetDevice> {
        Some(TakenNetDevice {
            name: self.name,
            prepared_device: self.device.take()?,
            dma: self.dma.take()?,
            irq_sources: self.info.irq_sources().to_vec(),
        })
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }
}

/// A platform network device removed exactly once from the probe registry.
pub struct TakenNetDevice {
    /// Stable platform registration name.
    pub name: &'static str,
    /// Portable device to consume into queue/control parts.
    pub prepared_device: Box<dyn NetDevice>,
    /// DMA capability used to allocate queue buffer pools.
    pub dma: DeviceDma,
    /// Complete driver source-id to platform IRQ mapping.
    pub irq_sources: alloc::vec::Vec<crate::BindingIrqBinding>,
}

/// Removes one registered platform network device.
pub fn take_net_device(device: Device<PlatformNetDevice>) -> Result<TakenNetDevice, NetError> {
    let mut dev = device
        .lock()
        .map_err(|_| NetError::Other(Box::new(rd_net::KError::Unknown("device locked"))))?;
    dev.take_net()
        .ok_or_else(|| NetError::Other(Box::new(rd_net::KError::Unknown("device already taken"))))
}

impl DriverGeneric for PlatformNetDevice {
    fn name(&self) -> &str {
        self.name
    }
}

impl BoundDevice for PlatformNetDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

pub trait PlatformDeviceNet {
    fn register_net<T>(self, name: &'static str, dev: T, dma: DeviceDma) -> Option<usize>
    where
        T: NetDevice + 'static;

    fn register_net_with_info<T>(
        self,
        name: &'static str,
        dev: T,
        dma: DeviceDma,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: NetDevice + 'static;
}

impl PlatformDeviceNet for rdrive::PlatformDevice {
    fn register_net<T>(self, name: &'static str, dev: T, dma: DeviceDma) -> Option<usize>
    where
        T: NetDevice + 'static,
    {
        self.register_net_with_info(name, dev, dma, BindingInfo::empty())
    }

    fn register_net_with_info<T>(
        self,
        name: &'static str,
        dev: T,
        dma: DeviceDma,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: NetDevice + 'static,
    {
        register_net_with_info(self, name, dev, dma, info)
    }
}

pub trait ProbeFdtNet {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: NetDevice + 'static;
}

impl ProbeFdtNet for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: NetDevice + 'static,
    {
        let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            crate::binding_resolver::dma_coherency_from_fdt(self.info()),
            dma_api::DmaConstraints::new(u64::MAX),
        ));
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_net_with_info(
            self.into_platform_device(),
            name,
            dev,
            dma,
            info,
        ))
    }
}

pub trait ProbeAcpiNet {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: NetDevice + 'static;
}

impl ProbeAcpiNet for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: NetDevice + 'static,
    {
        let dma = axklib::dma::device(dma_api::DmaDeviceInfo::new(
            dma_api::DmaDomainId::Direct,
            crate::binding_resolver::dma_coherency_from_acpi(self.info())?,
            dma_api::DmaConstraints::new(u64::MAX),
        ));
        let info = binding_info_from_acpi(self.info())?;
        Ok(register_net_with_info(
            self.into_platform_device(),
            name,
            dev,
            dma,
            info,
        ))
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciNet {
    fn register_net<T>(
        self,
        name: &'static str,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: NetDevice + 'static;
}

#[cfg(feature = "pci")]
impl ProbePciNet for rdrive::probe::pci::ProbePci<'_> {
    fn register_net<T>(
        self,
        name: &'static str,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: NetDevice + 'static,
    {
        let dma = crate::pci::device_dma(self.info(), u64::MAX);
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_net_with_info(
            self.into_platform_device(),
            name,
            dev,
            dma,
            info,
        ))
    }
}

fn register_net_with_info<T>(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    dev: T,
    dma: DeviceDma,
    info: BindingInfo,
) -> Option<usize>
where
    T: NetDevice + 'static,
{
    register_bound_device(
        plat_dev,
        PlatformNetDevice::new(name, Box::new(dev), dma, info),
    )
}
