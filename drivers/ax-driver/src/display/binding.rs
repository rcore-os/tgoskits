use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::AxError;
use rdif_display::Interface;
use rdrive::{DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, BindingIrq, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, TakeRegistered, register_bound_device, take_registered_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

pub struct PlatformDisplayDevice {
    name: String,
    info: BindingInfo,
    display: Option<Box<dyn Interface>>,
}

impl PlatformDisplayDevice {
    fn new(name: String, display: Box<dyn Interface>, info: BindingInfo) -> Self {
        Self {
            name,
            info,
            display: Some(display),
        }
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }

    pub fn irq_cloned(&self) -> Option<BindingIrq> {
        self.info.irq_cloned()
    }
}

impl DriverGeneric for PlatformDisplayDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformDisplayDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

pub struct TakenDisplayDevice {
    pub device: Box<dyn Interface>,
    pub irq: Option<BindingIrq>,
}

impl TakeRegistered for PlatformDisplayDevice {
    type Output = TakenDisplayDevice;

    fn take_registered(&mut self) -> Option<Self::Output> {
        Some(TakenDisplayDevice {
            device: self.display.take()?,
            irq: self.info.irq_cloned(),
        })
    }
}

pub trait PlatformDeviceDisplay {
    fn register_display<T>(self, dev: T) -> Option<usize>
    where
        T: Interface + 'static;

    fn register_display_with_info<T>(self, dev: T, info: BindingInfo) -> Option<usize>
    where
        T: Interface + 'static;
}

impl PlatformDeviceDisplay for rdrive::PlatformDevice {
    fn register_display<T>(self, dev: T) -> Option<usize>
    where
        T: Interface + 'static,
    {
        self.register_display_with_info(dev, BindingInfo::empty())
    }

    fn register_display_with_info<T>(self, dev: T, info: BindingInfo) -> Option<usize>
    where
        T: Interface + 'static,
    {
        register_display_with_info(self, dev, info)
    }
}

pub trait ProbeFdtDisplay {
    fn register_display<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeFdtDisplay for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_display<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_display_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

pub trait ProbeAcpiDisplay {
    fn register_display<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeAcpiDisplay for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_display<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_acpi(self.info())?;
        Ok(register_display_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciDisplay {
    fn register_display<T>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

#[cfg(feature = "pci")]
impl ProbePciDisplay for rdrive::probe::pci::ProbePci<'_> {
    fn register_display<T>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_display_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

fn register_display_with_info<T>(
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
        PlatformDisplayDevice::new(name, Box::new(dev), info),
    )
}

pub fn take_display_devices() -> Result<Vec<TakenDisplayDevice>, AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformDisplayDevice>() {
        let display = take_display_device(dev)?;
        devices.push(display);
    }
    Ok(devices)
}

fn take_display_device(
    device: rdrive::Device<PlatformDisplayDevice>,
) -> Result<TakenDisplayDevice, AxError> {
    take_registered_device(device).ok_or(AxError::BadState)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rdif_display::{DisplayError, DisplayInfo, FrameBuffer, PixelFormat};

    use super::*;
    use crate::{BindingInfo, BindingIrq};

    struct TestDisplay {
        fb: [u8; 16],
    }

    impl DriverGeneric for TestDisplay {
        fn name(&self) -> &str {
            "test-display"
        }
    }

    impl Interface for TestDisplay {
        fn info(&self) -> Result<DisplayInfo, DisplayError> {
            Ok(DisplayInfo {
                width: 2,
                height: 2,
                stride: 8,
                format: PixelFormat::Xrgb8888,
                fb_size: self.fb.len(),
            })
        }

        fn framebuffer(&mut self) -> Result<FrameBuffer<'_>, DisplayError> {
            Ok(FrameBuffer::from_slice(&mut self.fb))
        }
    }

    #[test]
    fn platform_display_device_exposes_binding_info_irq_num() {
        let irq = 42;
        let device = PlatformDisplayDevice::new(
            "test-display".into(),
            Box::new(TestDisplay { fb: [0; 16] }),
            BindingInfo::with_irq(Some(irq)).unwrap(),
        );

        assert_eq!(device.binding_info().irq_num(), Some(irq));
        assert_eq!(device.irq_num(), Some(irq));
        assert_eq!(BoundDevice::irq_num(&device), Some(irq));
    }

    #[test]
    fn platform_display_device_empty_binding_has_no_irq_num() {
        let device = PlatformDisplayDevice::new(
            "test-display".into(),
            Box::new(TestDisplay { fb: [0; 16] }),
            BindingInfo::empty(),
        );

        assert_eq!(device.binding_info().irq_num(), None);
        assert_eq!(device.irq_num(), None);
        assert_eq!(BoundDevice::irq_num(&device), None);
    }

    #[test]
    fn platform_display_device_exposes_native_binding_irq() {
        let irq = BindingIrq::fdt_interrupt_with_controller(rdrive::DeviceId::new(), [0, 42, 4]);
        let device = PlatformDisplayDevice::new(
            "test-display".into(),
            Box::new(TestDisplay { fb: [0; 16] }),
            BindingInfo::with_binding_irq(Some(irq.clone())),
        );

        assert_eq!(device.irq_cloned(), Some(irq));
        assert_eq!(device.irq_num(), None);
        assert_eq!(BoundDevice::irq_num(&device), None);
    }
}
