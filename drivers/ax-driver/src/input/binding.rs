use alloc::{boxed::Box, string::String, vec::Vec};

use rdif_input::Interface;
use rdrive::{DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, BindingIrq, Error, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, TakeRegistered, register_bound_device, take_registered_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

pub struct PlatformInputDevice {
    name: String,
    info: BindingInfo,
    input: Option<Box<dyn Interface>>,
}

impl PlatformInputDevice {
    fn new(name: String, input: Box<dyn Interface>, info: BindingInfo) -> Self {
        Self {
            name,
            info,
            input: Some(input),
        }
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }

    pub fn irq(&self) -> Option<&BindingIrq> {
        self.info.irq()
    }

    pub fn irq_cloned(&self) -> Option<BindingIrq> {
        self.info.irq_cloned()
    }
}

impl DriverGeneric for PlatformInputDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformInputDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

pub struct TakenInputDevice {
    pub device: Box<dyn Interface>,
    pub irq: Option<BindingIrq>,
}

impl TakeRegistered for PlatformInputDevice {
    type Output = TakenInputDevice;

    fn take_registered(&mut self) -> Option<Self::Output> {
        Some(TakenInputDevice {
            device: self.input.take()?,
            irq: self.info.irq_cloned(),
        })
    }
}

pub trait PlatformDeviceInput {
    fn register_input<T>(self, dev: T) -> Option<usize>
    where
        T: Interface + 'static;

    fn register_input_with_info<T>(self, dev: T, info: BindingInfo) -> Option<usize>
    where
        T: Interface + 'static;
}

impl PlatformDeviceInput for rdrive::PlatformDevice {
    fn register_input<T>(self, dev: T) -> Option<usize>
    where
        T: Interface + 'static,
    {
        self.register_input_with_info(dev, BindingInfo::empty())
    }

    fn register_input_with_info<T>(self, dev: T, info: BindingInfo) -> Option<usize>
    where
        T: Interface + 'static,
    {
        register_input_with_info(self, dev, info)
    }
}

pub trait ProbeFdtInput {
    fn register_input<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeFdtInput for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_input<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_input_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

pub trait ProbeAcpiInput {
    fn register_input<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeAcpiInput for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_input<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_acpi(self.info())?;
        Ok(register_input_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciInput {
    fn register_input<T>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

#[cfg(feature = "pci")]
impl ProbePciInput for rdrive::probe::pci::ProbePci<'_> {
    fn register_input<T>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_input_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

fn register_input_with_info<T>(
    plat_dev: rdrive::PlatformDevice,
    dev: T,
    info: BindingInfo,
) -> Option<usize>
where
    T: Interface + 'static,
{
    let name = dev.name().into();
    register_bound_device(
        plat_dev,
        PlatformInputDevice::new(name, Box::new(dev), info),
    )
}

pub fn take_input_devices() -> crate::Result<Vec<TakenInputDevice>> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformInputDevice>() {
        devices.push(take_input_device(dev)?);
    }
    Ok(devices)
}

fn take_input_device(
    device: rdrive::Device<PlatformInputDevice>,
) -> crate::Result<TakenInputDevice> {
    take_registered_device(device).ok_or(Error::DeviceUnavailable)
}
