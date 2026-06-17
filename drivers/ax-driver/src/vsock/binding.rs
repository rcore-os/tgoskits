use alloc::{boxed::Box, string::String, vec::Vec};

use ax_errno::AxError;
use rdif_vsock::Interface;
use rdrive::{DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, TakeRegistered, register_bound_device, take_registered_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

pub struct PlatformVsockDevice {
    name: String,
    info: BindingInfo,
    vsock: Option<Box<dyn Interface>>,
}

impl PlatformVsockDevice {
    fn new(name: String, vsock: Box<dyn Interface>, info: BindingInfo) -> Self {
        Self {
            name,
            info,
            vsock: Some(vsock),
        }
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }
}

impl DriverGeneric for PlatformVsockDevice {
    fn name(&self) -> &str {
        &self.name
    }
}

impl BoundDevice for PlatformVsockDevice {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

impl TakeRegistered for PlatformVsockDevice {
    type Output = Box<dyn Interface>;

    fn take_registered(&mut self) -> Option<Self::Output> {
        self.vsock.take()
    }
}

pub trait PlatformDeviceVsock {
    fn register_vsock<T>(self, dev: T) -> Option<usize>
    where
        T: Interface + 'static;

    fn register_vsock_with_info<T>(self, dev: T, info: BindingInfo) -> Option<usize>
    where
        T: Interface + 'static;
}

impl PlatformDeviceVsock for rdrive::PlatformDevice {
    fn register_vsock<T>(self, dev: T) -> Option<usize>
    where
        T: Interface + 'static,
    {
        self.register_vsock_with_info(dev, BindingInfo::empty())
    }

    fn register_vsock_with_info<T>(self, dev: T, info: BindingInfo) -> Option<usize>
    where
        T: Interface + 'static,
    {
        register_vsock_with_info(self, dev, info)
    }
}

pub trait ProbeFdtVsock {
    fn register_vsock<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeFdtVsock for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_vsock<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_vsock_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

pub trait ProbeAcpiVsock {
    fn register_vsock<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeAcpiVsock for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_vsock<T>(self, dev: T) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_acpi(self.info())?;
        Ok(register_vsock_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciVsock {
    fn register_vsock<T>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static;
}

#[cfg(feature = "pci")]
impl ProbePciVsock for rdrive::probe::pci::ProbePci<'_> {
    fn register_vsock<T>(
        self,
        dev: T,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_pci(self.info(), requirement)?;
        Ok(register_vsock_with_info(
            self.into_platform_device(),
            dev,
            info,
        ))
    }
}

fn register_vsock_with_info<T>(
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
        PlatformVsockDevice::new(name, Box::new(dev), info),
    )
}

pub fn take_vsock_devices() -> Result<Vec<Box<dyn Interface>>, AxError> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformVsockDevice>() {
        devices.push(take_vsock_device(dev)?);
    }
    Ok(devices)
}

fn take_vsock_device(
    device: rdrive::Device<PlatformVsockDevice>,
) -> Result<Box<dyn Interface>, AxError> {
    take_registered_device(device).ok_or(AxError::BadState)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rdif_vsock::{VsockConnId, VsockError, VsockEvent};

    use super::*;
    use crate::BindingInfo;

    struct TestVsock;

    impl DriverGeneric for TestVsock {
        fn name(&self) -> &str {
            "test-vsock"
        }
    }

    impl Interface for TestVsock {
        fn guest_cid(&self) -> u64 {
            3
        }

        fn listen(&mut self, _port: u32) -> Result<(), VsockError> {
            Ok(())
        }

        fn connect(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn send(&mut self, _id: VsockConnId, buf: &[u8]) -> Result<usize, VsockError> {
            Ok(buf.len())
        }

        fn recv(&mut self, _id: VsockConnId, _buf: &mut [u8]) -> Result<usize, VsockError> {
            Ok(0)
        }

        fn recv_avail(&mut self, _id: VsockConnId) -> Result<usize, VsockError> {
            Ok(0)
        }

        fn disconnect(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn abort(&mut self, _id: VsockConnId) -> Result<(), VsockError> {
            Ok(())
        }

        fn poll_event(&mut self) -> Result<Option<VsockEvent>, VsockError> {
            Ok(None)
        }
    }

    #[test]
    fn platform_vsock_device_exposes_binding_info_irq_num() {
        let irq = 44;
        let device = PlatformVsockDevice::new(
            "test-vsock".into(),
            Box::new(TestVsock),
            BindingInfo::with_irq(Some(irq)),
        );

        assert_eq!(device.binding_info().irq_num(), Some(irq));
        assert_eq!(device.irq_num(), Some(irq));
        assert_eq!(BoundDevice::irq_num(&device), Some(irq));
    }

    #[test]
    fn platform_vsock_device_empty_binding_has_no_irq_num() {
        let device = PlatformVsockDevice::new(
            "test-vsock".into(),
            Box::new(TestVsock),
            BindingInfo::empty(),
        );

        assert_eq!(device.binding_info().irq_num(), None);
        assert_eq!(device.irq_num(), None);
        assert_eq!(BoundDevice::irq_num(&device), None);
    }
}
