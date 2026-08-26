extern crate alloc;

use crab_usb::{EventHandler, USBHost, usb_if::Speed};
#[cfg(any(
    test,
    feature = "rockchip-dwc-xhci",
    feature = "rockchip-ehci",
    feature = "sg2002-dwc2",
    feature = "xhci-mmio"
))]
use dma_api::{DeviceDma, DmaCoherency, DmaConstraints, DmaDeviceInfo, DmaDomainId};
use rdrive::{DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, BindingIrq, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

#[cfg(feature = "rockchip-dwc-xhci")]
mod dwc;
#[cfg(feature = "rockchip-ehci")]
mod ehci;
#[cfg(feature = "sg2002-dwc2")]
mod sg2002_dwc2;
#[cfg(feature = "xhci-mmio")]
mod xhci_mmio;
#[cfg(feature = "xhci-pci")]
mod xhci_pci;

pub type UsbHostDevice = rdrive::Device<PlatformUsbHost>;
pub type UsbHostDeviceGuard = rdrive::DeviceGuard<PlatformUsbHost>;

#[cfg(any(
    feature = "rockchip-dwc-xhci",
    feature = "rockchip-ehci",
    feature = "sg2002-dwc2",
    feature = "xhci-mmio",
    feature = "xhci-pci"
))]
mod runtime {
    use core::time::Duration;

    struct UsbRuntime;

    impl crab_usb::KernelOp for UsbRuntime {
        fn delay(&self, duration: Duration) {
            axklib::time::busy_wait(duration);
        }
    }

    static USB_RUNTIME: UsbRuntime = UsbRuntime;

    pub(crate) fn usb_runtime() -> &'static dyn crab_usb::KernelOp {
        &USB_RUNTIME
    }
}

#[cfg(any(
    feature = "rockchip-dwc-xhci",
    feature = "rockchip-ehci",
    feature = "sg2002-dwc2",
    feature = "xhci-mmio",
    feature = "xhci-pci"
))]
pub(crate) use runtime::usb_runtime;

#[cfg(any(
    feature = "rockchip-dwc-xhci",
    feature = "rockchip-ehci",
    feature = "sg2002-dwc2",
    feature = "xhci-mmio"
))]
pub(crate) fn usb_device_dma(coherency: DmaCoherency) -> DeviceDma {
    axklib::dma::device(DmaDeviceInfo::new(
        DmaDomainId::Direct,
        coherency,
        DmaConstraints::new(u64::MAX),
    ))
}

pub struct PlatformUsbHost {
    name: &'static str,
    info: BindingInfo,
    host: USBHost,
    root_hub_speed: Speed,
    irq_handler_taken: bool,
}

impl PlatformUsbHost {
    fn try_new(name: &'static str, host: USBHost, info: BindingInfo) -> Result<Self, OnProbeError> {
        Self::try_new_with_root_hub_speed(name, host, info, Speed::SuperSpeedPlus)
    }

    fn try_new_with_root_hub_speed(
        name: &'static str,
        host: USBHost,
        info: BindingInfo,
        root_hub_speed: Speed,
    ) -> Result<Self, OnProbeError> {
        if info.irq_cloned().is_none() {
            return Err(OnProbeError::other(alloc::format!(
                "USB host {name} has no interrupt binding"
            )));
        }
        Ok(Self {
            name,
            info,
            host,
            root_hub_speed,
            irq_handler_taken: false,
        })
    }

    pub fn host(&self) -> &USBHost {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut USBHost {
        &mut self.host
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn root_hub_speed(&self) -> Speed {
        self.root_hub_speed
    }

    pub fn enable_irq(&mut self) -> crab_usb::err::Result {
        self.host.enable_irq()
    }

    pub fn disable_irq(&mut self) -> crab_usb::err::Result {
        self.host.disable_irq()
    }

    pub fn take_binding_irq_handler(&mut self) -> Option<(BindingIrq, UsbHostIrqHandler)> {
        let irq = self.info.irq_cloned()?;
        if self.irq_handler_taken {
            return None;
        }

        self.irq_handler_taken = true;
        let handler = UsbHostIrqHandler::new(self.host.create_event_handler());
        Some((irq, handler))
    }
}

impl DriverGeneric for PlatformUsbHost {
    fn name(&self) -> &str {
        self.name
    }
}

impl BoundDevice for PlatformUsbHost {
    fn binding_info(&self) -> &BindingInfo {
        &self.info
    }
}

pub struct UsbHostIrqHandler {
    handler: EventHandler,
}

impl UsbHostIrqHandler {
    fn new(handler: EventHandler) -> Self {
        Self { handler }
    }

    /// Acknowledges and masks one device interrupt with bounded register work.
    pub fn acknowledge_irq(&self) -> bool {
        self.handler.acknowledge_irq()
    }

    /// Drains one event batch outside hard-IRQ context.
    pub fn drain_event(&self) -> crab_usb::Event {
        self.handler.drain_event()
    }

    /// Rearms the device interrupt after task-context draining.
    pub fn rearm_irq(&self) {
        self.handler.rearm_irq()
    }

    /// Polls and drains one event batch in task context.
    pub fn handle(&self) -> crab_usb::Event {
        self.handler.handle_event()
    }
}

pub trait ProbeFdtUsbHost {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
    ) -> Result<Option<usize>, OnProbeError>;

    fn register_usb_host_with_root_hub_speed(
        self,
        name: &'static str,
        host: USBHost,
        root_hub_speed: Speed,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeFdtUsbHost for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_fdt(self.info())?;
        register_usb_host_with_info(self.into_platform_device(), name, host, info)
    }

    fn register_usb_host_with_root_hub_speed(
        self,
        name: &'static str,
        host: USBHost,
        root_hub_speed: Speed,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_fdt(self.info())?;
        register_usb_host_with_info_and_root_hub_speed(
            self.into_platform_device(),
            name,
            host,
            info,
            root_hub_speed,
        )
    }
}

pub trait ProbeAcpiUsbHost {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeAcpiUsbHost for rdrive::probe::acpi::ProbeAcpi<'_> {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_acpi(self.info())?;
        register_usb_host_with_info(self.into_platform_device(), name, host, info)
    }
}

#[cfg(feature = "pci")]
pub trait ProbePciUsbHost {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError>;
}

#[cfg(feature = "pci")]
impl ProbePciUsbHost for rdrive::probe::pci::ProbePci<'_> {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
        requirement: PciIrqRequirement,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_pci(self.info(), requirement)?;
        register_usb_host_with_info(self.into_platform_device(), name, host, info)
    }
}

fn register_usb_host_with_info(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    host: USBHost,
    info: BindingInfo,
) -> Result<Option<usize>, OnProbeError> {
    Ok(register_bound_device(
        plat_dev,
        PlatformUsbHost::try_new(name, host, info)?,
    ))
}

fn register_usb_host_with_info_and_root_hub_speed(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    host: USBHost,
    info: BindingInfo,
    root_hub_speed: Speed,
) -> Result<Option<usize>, OnProbeError> {
    Ok(register_bound_device(
        plat_dev,
        PlatformUsbHost::try_new_with_root_hub_speed(name, host, info, root_hub_speed)?,
    ))
}

#[cfg(feature = "xhci-pci")]
pub(crate) fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}

pub fn usb_host_device() -> Option<UsbHostDevice> {
    rdrive::get_one()
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec};
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

    use crab_usb::{Dwc2HostParams, Dwc2NewParams, USBHost, usb_if::Speed};
    use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};

    use super::*;

    struct TestUsbKernel;

    impl DmaOp for TestUsbKernel {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            _layout: Layout,
        ) -> Option<DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_contiguous(&self, _handle: DmaAllocHandle) {}

        unsafe fn alloc_coherent(
            &self,
            _constraints: DmaConstraints,
            _layout: Layout,
        ) -> Option<DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_coherent(&self, _handle: DmaAllocHandle) -> Result<(), DmaError> {
            Ok(())
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            _addr: NonNull<u8>,
            _size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            Err(DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    impl crab_usb::KernelOp for TestUsbKernel {
        fn delay(&self, _duration: core::time::Duration) {}
    }

    static TEST_USB_KERNEL: TestUsbKernel = TestUsbKernel;

    fn test_usb_host() -> USBHost {
        let regs = Box::leak(vec![0u32; 1024].into_boxed_slice());
        let mmio = NonNull::new(regs.as_mut_ptr().cast::<u8>()).unwrap();
        USBHost::new_dwc2(Dwc2NewParams {
            mmio,
            dma: DeviceDma::new(
                DmaDeviceInfo::new(
                    DmaDomainId::Direct,
                    DmaCoherency::NonCoherent,
                    DmaConstraints::new(u64::MAX),
                ),
                &TEST_USB_KERNEL,
            ),
            kernel: &TEST_USB_KERNEL,
            params: Dwc2HostParams::sg2002(),
        })
        .unwrap()
    }

    #[test]
    fn binding_irq_handler_preserves_fdt_interrupt_binding() {
        let binding =
            BindingIrq::fdt_interrupt_with_controller(rdrive::DeviceId::new(), [0, 30, 4]);
        let info = BindingInfo::with_binding_irq(Some(binding.clone()));
        let mut host = PlatformUsbHost::try_new_with_root_hub_speed(
            "test-usb",
            test_usb_host(),
            info,
            Speed::High,
        )
        .expect("test host should accept an interrupt binding");

        let (actual, _handler) = host
            .take_binding_irq_handler()
            .expect("binding IRQ handler should be available");
        assert_eq!(actual, binding);
        assert!(host.take_binding_irq_handler().is_none());
    }

    #[test]
    fn usb_host_rejects_missing_irq_binding() {
        let result = PlatformUsbHost::try_new("test-usb", test_usb_host(), BindingInfo::empty());

        assert!(
            result.is_err(),
            "an event-driven USB host must not silently fall back to polling"
        );
    }
}
