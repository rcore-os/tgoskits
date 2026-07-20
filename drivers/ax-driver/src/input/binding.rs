use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::AxError;
use rdif_input::Interface;
use rdrive::{DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, BindingIrq, binding_info_from_acpi, binding_info_from_fdt,
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

pub fn take_input_devices() -> Result<Vec<TakenInputDevice>, AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformInputDevice>() {
        devices.push(take_input_device(dev)?);
    }
    Ok(devices)
}

fn take_input_device(
    device: rdrive::Device<PlatformInputDevice>,
) -> Result<TakenInputDevice, AxError> {
    take_registered_device(device).ok_or(AxError::BadState)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rdif_input::{EventType, InputDeviceId, InputError, InputEvent};

    use super::*;
    use crate::{BindingInfo, BindingIrq};

    struct TestInput;

    impl DriverGeneric for TestInput {
        fn name(&self) -> &str {
            "test-input"
        }
    }

    impl Interface for TestInput {
        fn enable_irq(&mut self) -> Result<(), InputError> {
            Ok(())
        }

        fn disable_irq(&mut self) -> Result<(), InputError> {
            Ok(())
        }

        fn is_irq_enabled(&self) -> bool {
            false
        }

        fn device_id(&self) -> InputDeviceId {
            InputDeviceId {
                bus_type: 3,
                vendor: 1,
                product: 2,
                version: 1,
            }
        }

        fn physical_location(&self) -> &str {
            "test/input0"
        }

        fn unique_id(&self) -> &str {
            "input0"
        }

        fn get_event_bits(&mut self, _ty: EventType, out: &mut [u8]) -> Result<bool, InputError> {
            if let Some(first) = out.first_mut() {
                *first = 1;
            }
            Ok(!out.is_empty())
        }

        fn read_event(&mut self) -> Result<InputEvent, InputError> {
            Ok(InputEvent {
                event_type: EventType::Key as u16,
                code: 30,
                value: 1,
            })
        }
    }

    #[test]
    fn platform_input_device_exposes_binding_info_irq_num() {
        let irq = 43;
        let device = PlatformInputDevice::new(
            "test-input".into(),
            Box::new(TestInput),
            BindingInfo::with_irq(Some(irq)).unwrap(),
        );

        assert_eq!(device.binding_info().irq_num(), Some(irq));
        assert_eq!(device.irq_num(), Some(irq));
        assert_eq!(BoundDevice::irq_num(&device), Some(irq));
    }

    #[test]
    fn platform_input_device_empty_binding_has_no_irq_num() {
        let device = PlatformInputDevice::new(
            "test-input".into(),
            Box::new(TestInput),
            BindingInfo::empty(),
        );

        assert_eq!(device.binding_info().irq_num(), None);
        assert_eq!(device.irq_num(), None);
        assert_eq!(BoundDevice::irq_num(&device), None);
    }

    #[test]
    fn platform_input_device_exposes_native_binding_irq() {
        let irq = BindingIrq::fdt_interrupt_with_controller(rdrive::DeviceId::new(), [0, 42, 4]);
        let device = PlatformInputDevice::new(
            "test-input".into(),
            Box::new(TestInput),
            BindingInfo::with_binding_irq(Some(irq.clone())),
        );

        assert_eq!(device.irq_cloned(), Some(irq));
        assert_eq!(device.irq_num(), None);
    }
}
