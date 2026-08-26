use alloc::{boxed::Box, format, string::String, vec::Vec};

use rdif_vsock::{Interface, VsockIrqEndpoints};
use rdrive::{DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, Error, binding_info_from_acpi, binding_info_from_fdt,
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
    type Output = TakenVsockDevice;

    fn take_registered(&mut self) -> Option<Self::Output> {
        let [binding] = self.info.irq_sources() else {
            return None;
        };
        let irq = binding.irq.clone();
        let endpoints = self.vsock.as_mut()?.take_irq_endpoints().ok()?;
        Some(TakenVsockDevice {
            name: self.name.clone(),
            device: self.vsock.take()?,
            irq,
            endpoints,
        })
    }
}

/// A vsock device transferred with its single IRQ binding and capabilities.
pub struct TakenVsockDevice {
    pub name: String,
    pub device: Box<dyn Interface>,
    pub irq: crate::BindingIrq,
    pub endpoints: VsockIrqEndpoints,
}

pub trait PlatformDeviceVsock {
    fn register_vsock_with_info<T>(self, dev: T, info: BindingInfo) -> Result<(), OnProbeError>
    where
        T: Interface + 'static;
}

impl PlatformDeviceVsock for rdrive::PlatformDevice {
    fn register_vsock_with_info<T>(self, dev: T, info: BindingInfo) -> Result<(), OnProbeError>
    where
        T: Interface + 'static,
    {
        register_vsock_with_info(self, dev, info)
    }
}

pub trait ProbeFdtVsock {
    fn register_vsock<T>(self, dev: T) -> Result<(), OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeFdtVsock for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_vsock<T>(self, dev: T) -> Result<(), OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_fdt(self.info())?;
        register_vsock_with_info(self.into_platform_device(), dev, info)
    }
}

pub trait ProbeAcpiVsock {
    fn register_vsock<T>(self, dev: T) -> Result<(), OnProbeError>
    where
        T: Interface + 'static;
}

impl ProbeAcpiVsock for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_vsock<T>(self, dev: T) -> Result<(), OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_acpi(self.info())?;
        register_vsock_with_info(self.into_platform_device(), dev, info)
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciVsock {
    fn register_vsock<T>(self, dev: T, requirement: PciIrqRequirement) -> Result<(), OnProbeError>
    where
        T: Interface + 'static;
}

#[cfg(feature = "pci")]
impl ProbePciVsock for rdrive::probe::pci::ProbePci<'_> {
    fn register_vsock<T>(self, dev: T, requirement: PciIrqRequirement) -> Result<(), OnProbeError>
    where
        T: Interface + 'static,
    {
        let info = binding_info_from_pci(self.info(), requirement)?;
        register_vsock_with_info(self.into_platform_device(), dev, info)
    }
}

fn register_vsock_with_info<T>(
    plat_dev: rdrive::PlatformDevice,
    dev: T,
    info: BindingInfo,
) -> Result<(), OnProbeError>
where
    T: Interface + 'static,
{
    let name = dev.name().into();
    if info.irq_sources().len() != 1 {
        return Err(OnProbeError::other(format!(
            "vsock device {name} requires exactly one IRQ binding"
        )));
    }
    register_bound_device(
        plat_dev,
        PlatformVsockDevice::new(name, Box::new(dev), info),
    );
    Ok(())
}

pub fn take_vsock_devices() -> crate::Result<Vec<TakenVsockDevice>> {
    let mut devices = Vec::new();
    for dev in rdrive::get_list::<PlatformVsockDevice>() {
        devices.push(take_vsock_device(dev)?);
    }
    Ok(devices)
}

fn take_vsock_device(
    device: rdrive::Device<PlatformVsockDevice>,
) -> crate::Result<TakenVsockDevice> {
    take_registered_device(device).ok_or(Error::DeviceUnavailable)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use rdif_vsock::{
        VsockConnId, VsockError, VsockEvent, VsockHardIrqEndpoint, VsockHardIrqHandler,
        VsockHardIrqResult, VsockPollIrqControl, VsockRearmResult,
    };

    use super::*;
    use crate::BindingInfo;

    struct TestHardIrq;

    impl VsockHardIrqHandler for TestHardIrq {
        fn handle_irq(&mut self) -> VsockHardIrqResult {
            VsockHardIrqResult::Spurious
        }
    }

    struct TestIrqControl;

    impl VsockPollIrqControl for TestIrqControl {
        fn quiesce(&mut self) -> Result<(), VsockError> {
            Ok(())
        }

        fn rearm_and_check(&mut self) -> Result<VsockRearmResult, VsockError> {
            Ok(VsockRearmResult::Idle)
        }

        fn shutdown(&mut self) -> Result<(), VsockError> {
            Ok(())
        }
    }

    struct TestVsock {
        irq_endpoints: Option<VsockIrqEndpoints>,
    }

    impl TestVsock {
        fn new() -> Self {
            Self {
                irq_endpoints: Some(VsockIrqEndpoints::new(
                    VsockHardIrqEndpoint::new(Box::new(TestHardIrq)),
                    Box::new(TestIrqControl),
                )),
            }
        }
    }

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

        fn send_capacity(&mut self, _id: VsockConnId) -> Result<usize, VsockError> {
            Ok(usize::MAX)
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

        fn take_irq_endpoints(&mut self) -> Result<VsockIrqEndpoints, VsockError> {
            self.irq_endpoints.take().ok_or(VsockError::NotAvailable)
        }
    }

    #[test]
    fn platform_vsock_device_exposes_binding_info_irq_num() {
        let irq = 44;
        let device = PlatformVsockDevice::new(
            "test-vsock".into(),
            Box::new(TestVsock::new()),
            BindingInfo::with_irq(Some(irq)).unwrap(),
        );

        assert_eq!(device.binding_info().irq_num(), Some(irq));
        assert_eq!(device.irq_num(), Some(irq));
        assert_eq!(BoundDevice::irq_num(&device), Some(irq));
    }

    #[test]
    fn platform_vsock_device_without_irq_cannot_transfer_runtime_ownership() {
        let mut device = PlatformVsockDevice::new(
            "test-vsock".into(),
            Box::new(TestVsock::new()),
            BindingInfo::empty(),
        );

        assert_eq!(device.binding_info().irq_num(), None);
        assert!(
            device.take_registered().is_none(),
            "a vsock device without an IRQ binding must never reach the runtime"
        );
    }
}
