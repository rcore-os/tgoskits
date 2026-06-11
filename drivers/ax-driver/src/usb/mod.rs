extern crate alloc;

use core::time::Duration;

use crab_usb::{EventHandler, USBHost};
use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
use rdrive::{DriverGeneric, probe::OnProbeError};

use crate::{
    BindingInfo, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};
#[cfg(feature = "pci")]
use crate::{PciIrqRequirement, binding_info_from_pci};

#[cfg(feature = "rockchip-dwc-xhci")]
mod dwc;
#[cfg(feature = "xhci-mmio")]
mod xhci_mmio;
#[cfg(feature = "xhci-pci")]
mod xhci_pci;

pub type UsbHostDevice = rdrive::Device<PlatformUsbHost>;
pub type UsbHostDeviceGuard = rdrive::DeviceGuard<PlatformUsbHost>;

struct UsbKernel;

impl DmaOp for UsbKernel {
    fn page_size(&self) -> usize {
        axklib::dma::op().page_size()
    }

    unsafe fn alloc_contiguous(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { axklib::dma::op().alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
        unsafe { axklib::dma::op().dealloc_contiguous(handle) }
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: DmaConstraints,
        layout: core::alloc::Layout,
    ) -> Option<DmaAllocHandle> {
        unsafe { axklib::dma::op().alloc_coherent(constraints, layout) }
    }

    unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
        unsafe { axklib::dma::op().dealloc_coherent(handle) }
    }

    unsafe fn map_streaming(
        &self,
        constraints: DmaConstraints,
        addr: core::ptr::NonNull<u8>,
        size: core::num::NonZeroUsize,
        direction: DmaDirection,
    ) -> Result<DmaMapHandle, DmaError> {
        unsafe { axklib::dma::op().map_streaming(constraints, addr, size, direction) }
    }

    unsafe fn unmap_streaming(&self, handle: DmaMapHandle) {
        unsafe { axklib::dma::op().unmap_streaming(handle) }
    }

    fn flush(&self, addr: core::ptr::NonNull<u8>, size: usize) {
        axklib::dma::op().flush(addr, size);
    }

    fn invalidate(&self, addr: core::ptr::NonNull<u8>, size: usize) {
        axklib::dma::op().invalidate(addr, size);
    }

    fn flush_invalidate(&self, addr: core::ptr::NonNull<u8>, size: usize) {
        axklib::dma::op().flush_invalidate(addr, size);
    }

    fn sync_alloc_for_device(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        axklib::dma::op().sync_alloc_for_device(handle, offset, size, direction);
    }

    fn sync_alloc_for_cpu(
        &self,
        handle: &DmaAllocHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        axklib::dma::op().sync_alloc_for_cpu(handle, offset, size, direction);
    }

    fn sync_map_for_device(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        axklib::dma::op().sync_map_for_device(handle, offset, size, direction);
    }

    fn sync_map_for_cpu(
        &self,
        handle: &DmaMapHandle,
        offset: usize,
        size: usize,
        direction: DmaDirection,
    ) {
        axklib::dma::op().sync_map_for_cpu(handle, offset, size, direction);
    }
}

impl crab_usb::KernelOp for UsbKernel {
    fn delay(&self, duration: Duration) {
        axklib::time::busy_wait(duration);
    }
}

static USB_KERNEL: UsbKernel = UsbKernel;

pub fn usb_kernel() -> &'static dyn crab_usb::KernelOp {
    &USB_KERNEL
}

pub struct PlatformUsbHost {
    name: &'static str,
    info: BindingInfo,
    host: USBHost,
    irq_handler_taken: bool,
}

impl PlatformUsbHost {
    fn new(name: &'static str, host: USBHost, info: BindingInfo) -> Self {
        Self {
            name,
            info,
            host,
            irq_handler_taken: false,
        }
    }

    pub fn host(&self) -> &USBHost {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut USBHost {
        &mut self.host
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.info.irq_num()
    }

    pub fn binding_info(&self) -> &BindingInfo {
        &self.info
    }

    pub fn enable_irq(&mut self) -> crab_usb::err::Result {
        self.host.enable_irq()
    }

    pub fn disable_irq(&mut self) -> crab_usb::err::Result {
        self.host.disable_irq()
    }

    pub fn take_irq_handler(&mut self) -> Option<(usize, UsbHostIrqHandler)> {
        let irq = self.info.irq_num()?;
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

    pub fn handle(&self) -> crab_usb::Event {
        self.handler.handle_event()
    }
}

pub trait PlatformDeviceUsbHost {
    fn register_usb_host(self, name: &'static str, host: USBHost) -> Option<usize>;

    fn register_usb_host_with_info(
        self,
        name: &'static str,
        host: USBHost,
        info: BindingInfo,
    ) -> Option<usize>;
}

impl PlatformDeviceUsbHost for rdrive::PlatformDevice {
    fn register_usb_host(self, name: &'static str, host: USBHost) -> Option<usize> {
        self.register_usb_host_with_info(name, host, BindingInfo::empty())
    }

    fn register_usb_host_with_info(
        self,
        name: &'static str,
        host: USBHost,
        info: BindingInfo,
    ) -> Option<usize> {
        register_usb_host_with_info(self, name, host, info)
    }
}

pub trait ProbeFdtUsbHost {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
    ) -> Result<Option<usize>, OnProbeError>;
}

impl ProbeFdtUsbHost for rdrive::probe::fdt::ProbeFdt<'_> {
    fn register_usb_host(
        self,
        name: &'static str,
        host: USBHost,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_usb_host_with_info(
            self.into_platform_device(),
            name,
            host,
            info,
        ))
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
        Ok(register_usb_host_with_info(
            self.into_platform_device(),
            name,
            host,
            info,
        ))
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
        Ok(register_usb_host_with_info(
            self.into_platform_device(),
            name,
            host,
            info,
        ))
    }
}

fn register_usb_host_with_info(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    host: USBHost,
    info: BindingInfo,
) -> Option<usize> {
    register_bound_device(plat_dev, PlatformUsbHost::new(name, host, info))
}

#[cfg(feature = "xhci-pci")]
pub(crate) fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}

pub fn usb_host_device() -> Option<UsbHostDevice> {
    rdrive::get_one()
}
