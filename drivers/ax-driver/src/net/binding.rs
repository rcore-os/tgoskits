extern crate alloc;

use alloc::boxed::Box;

use rd_net::{Interface, NetError};
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
    net: Option<rd_net::Net>,
}

impl PlatformNetDevice {
    fn new(name: &'static str, net: rd_net::Net, info: BindingInfo) -> Self {
        Self {
            name,
            info,
            net: Some(net),
        }
    }

    pub fn take_net(&mut self) -> Option<(rd_net::Net, &'static str, Option<usize>)> {
        Some((self.net.take()?, self.name, self.info.irq_num()))
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }
}

pub fn take_rd_net_device(
    device: Device<PlatformNetDevice>,
) -> Result<(rd_net::Net, &'static str, Option<usize>), NetError> {
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
    fn register_net<T>(self, name: &'static str, dev: T) -> Option<usize>
    where
        T: Interface + 'static;

    fn register_net_with_info<T>(
        self,
        name: &'static str,
        dev: T,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: Interface + 'static;
}

impl PlatformDeviceNet for rdrive::PlatformDevice {
    fn register_net<T>(self, name: &'static str, dev: T) -> Option<usize>
    where
        T: Interface + 'static,
    {
        self.register_net_with_info(name, dev, BindingInfo::empty())
    }

    fn register_net_with_info<T>(
        self,
        name: &'static str,
        dev: T,
        info: BindingInfo,
    ) -> Option<usize>
    where
        T: Interface + 'static,
    {
        register_net_with_info(self, name, dev, info)
    }
}

pub trait ProbeFdtNet {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeFdtNet for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_net_with_info(
            self.into_platform_device(),
            name,
            dev,
            info,
        ))
    }
}

pub trait ProbeAcpiNet {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeAcpiNet for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_net<T>(self, name: &'static str, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_acpi(self.info())?;
        Ok(register_net_with_info(
            self.into_platform_device(),
            name,
            dev,
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
        T: Interface + 'static;
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
        T: Interface + 'static,
    {
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_net_with_info(
            self.into_platform_device(),
            name,
            dev,
            info,
        ))
    }
}

fn register_net_with_info<T>(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    dev: T,
    info: BindingInfo,
) -> Option<usize>
where
    T: Interface + 'static,
{
    let net = rd_net::Net::new(dev, axklib::dma::op());
    register_bound_device(plat_dev, PlatformNetDevice::new(name, net, info))
}
