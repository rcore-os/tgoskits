extern crate alloc;

#[cfg(target_os = "none")]
use core::time::Duration;

#[cfg(target_os = "none")]
use crab_usb::USBHost;
#[cfg(target_os = "none")]
use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
use rdrive::DriverGeneric;

#[cfg(all(feature = "rockchip-dwc-xhci", target_os = "none"))]
mod dwc;
#[cfg(all(feature = "xhci-mmio", target_os = "none"))]
mod xhci_mmio;
#[cfg(all(feature = "xhci-pci", target_os = "none"))]
mod xhci_pci;

pub type UsbHostDevice = rdrive::Device<PlatformUsbHost>;
pub type UsbHostDeviceGuard = rdrive::DeviceGuard<PlatformUsbHost>;

#[cfg(target_os = "none")]
struct UsbKernel;

#[cfg(target_os = "none")]
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

#[cfg(target_os = "none")]
impl crab_usb::KernelOp for UsbKernel {
    fn delay(&self, duration: Duration) {
        axklib::time::busy_wait(duration);
    }
}

#[cfg(target_os = "none")]
static USB_KERNEL: UsbKernel = UsbKernel;

#[cfg(target_os = "none")]
pub fn usb_kernel() -> &'static dyn crab_usb::KernelOp {
    &USB_KERNEL
}

#[cfg(target_os = "none")]
pub struct PlatformUsbHost {
    name: &'static str,
    irq_num: Option<usize>,
    host: USBHost,
}

#[cfg(not(target_os = "none"))]
pub struct PlatformUsbHost {
    name: &'static str,
    irq_num: Option<usize>,
}

impl PlatformUsbHost {
    #[cfg(target_os = "none")]
    fn new(name: &'static str, host: USBHost, irq_num: Option<usize>) -> Self {
        Self {
            name,
            irq_num,
            host,
        }
    }

    #[cfg(not(target_os = "none"))]
    fn new_stub(name: &'static str, irq_num: Option<usize>) -> Self {
        Self { name, irq_num }
    }

    #[cfg(target_os = "none")]
    pub fn host(&self) -> &USBHost {
        &self.host
    }

    #[cfg(target_os = "none")]
    pub fn host_mut(&mut self) -> &mut USBHost {
        &mut self.host
    }

    pub fn irq_num(&self) -> Option<usize> {
        self.irq_num
    }
}

impl DriverGeneric for PlatformUsbHost {
    fn name(&self) -> &str {
        self.name
    }
}

pub trait PlatformDeviceUsbHost {
    #[cfg(target_os = "none")]
    fn register_usb_host(self, name: &'static str, host: USBHost, irq_num: Option<usize>);

    #[cfg(not(target_os = "none"))]
    fn register_usb_host_stub(self, name: &'static str, irq_num: Option<usize>);
}

impl PlatformDeviceUsbHost for rdrive::PlatformDevice {
    #[cfg(target_os = "none")]
    fn register_usb_host(self, name: &'static str, host: USBHost, irq_num: Option<usize>) {
        self.register(PlatformUsbHost::new(name, host, irq_num));
    }

    #[cfg(not(target_os = "none"))]
    fn register_usb_host_stub(self, name: &'static str, irq_num: Option<usize>) {
        self.register(PlatformUsbHost::new_stub(name, irq_num));
    }
}

#[cfg(all(feature = "xhci-pci", target_os = "none"))]
pub(crate) fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}

pub fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
    let interrupt = interrupts.first()?;
    decode_irq_cells(&interrupt.specifier)
}

fn decode_irq_cells(specifier: &[u32]) -> Option<usize> {
    match specifier {
        [irq] => Some(*irq as usize),
        [kind, irq, ..] => match *kind {
            0 => Some(*irq as usize + 32),
            1 => Some(*irq as usize + 16),
            _ => Some(*irq as usize),
        },
        _ => None,
    }
}

#[cfg(all(feature = "xhci-pci", target_os = "none"))]
fn pci_static_irq(endpoint: &rdrive::probe::pci::EndpointRc) -> Option<usize> {
    let interrupt_pin = endpoint.interrupt_pin();
    if let Some(irq) = crate::pci::legacy_irq_for_endpoint(endpoint.address(), interrupt_pin) {
        return Some(irq);
    }
    let line = endpoint.interrupt_line();
    (line != 0 && line != u8::MAX).then_some(line as usize)
}

#[cfg(all(feature = "xhci-pci", target_os = "none"))]
pub(crate) fn pci_irq_or_error(
    endpoint: &rdrive::probe::pci::EndpointRc,
) -> Result<usize, rdrive::probe::OnProbeError> {
    #[cfg(feature = "pci-fdt")]
    if let Some(irq) =
        crate::pci::fdt_irq_for_endpoint(endpoint.address(), endpoint.interrupt_pin())?
    {
        return Ok(irq);
    }

    pci_static_irq(endpoint).ok_or_else(|| {
        rdrive::probe::OnProbeError::other(alloc::format!(
            "failed to resolve IRQ for USB endpoint {}",
            endpoint.address()
        ))
    })
}

pub fn usb_host_device() -> Option<UsbHostDevice> {
    rdrive::get_one()
}
