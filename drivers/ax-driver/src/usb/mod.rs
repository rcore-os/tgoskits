extern crate alloc;

use alloc::boxed::Box;
use core::time::Duration;

use crab_usb::{EventHandler, USBHost, usb_if::Speed};
use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
use rdif_irq::{ContainmentCause, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource};
use rdrive::{DriverGeneric, probe::OnProbeError};

#[cfg(any(feature = "xhci-pci", test))]
use crate::IrqBindingLease;
use crate::{
    BindingInfo, BindingIrq, IrqBindingError, binding_info_from_acpi, binding_info_from_fdt,
    registration::{BoundDevice, register_bound_device},
};
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
    irq_binding: UsbIrqBindingOwner,
    host: USBHost,
    root_hub_speed: Speed,
    irq_handler_taken: bool,
}

/// Failure to publish or withdraw a USB host interrupt source.
#[derive(Debug, thiserror::Error)]
pub enum UsbIrqLifecycleError {
    /// The device-local source could not be enabled and containment was incomplete.
    #[error(
        "USB device IRQ enable failed: {error}; device rollback: {device_rollback_error:?}; \
         binding rollback: {binding_rollback_error:?}"
    )]
    DeviceEnable {
        #[source]
        error: Box<crab_usb::err::USBError>,
        device_rollback_error: Option<Box<crab_usb::err::USBError>>,
        binding_rollback_error: Option<Box<IrqBindingError>>,
    },
    /// The outer platform gate could not be enabled after the device source was published.
    #[error(
        "USB IRQ binding enable failed: {error}; device rollback: {device_rollback_error:?}; \
         binding rollback: {binding_rollback_error:?}"
    )]
    BindingEnable {
        #[source]
        error: Box<IrqBindingError>,
        device_rollback_error: Option<Box<crab_usb::err::USBError>>,
        binding_rollback_error: Option<Box<IrqBindingError>>,
    },
    /// At least one shutdown transition failed after every containment step was attempted.
    #[error("USB IRQ disable failed; device source: {device_error:?}; binding: {binding_error:?}")]
    Disable {
        device_error: Option<Box<crab_usb::err::USBError>>,
        binding_error: Option<Box<IrqBindingError>>,
    },
}

/// Keeps immutable route metadata and an optional move-only endpoint gate together.
enum UsbIrqBindingOwner {
    /// Firmware/controller routing whose delivery gate is owned by the IRQ framework.
    PlatformRoute(BindingInfo),
    /// A bus endpoint gate that must remain retained for the complete host lifetime.
    #[cfg(any(feature = "xhci-pci", test))]
    EndpointGate {
        info: BindingInfo,
        lease: Box<dyn IrqBindingLease>,
    },
}

impl UsbIrqBindingOwner {
    const fn platform_route(info: BindingInfo) -> Self {
        Self::PlatformRoute(info)
    }

    #[cfg(any(feature = "xhci-pci", test))]
    fn endpoint_gate<L: IrqBindingLease>(lease: L) -> Self {
        let info = lease.binding_info();
        Self::EndpointGate {
            info,
            lease: Box::new(lease),
        }
    }

    const fn binding_info(&self) -> &BindingInfo {
        match self {
            Self::PlatformRoute(info) => info,
            #[cfg(any(feature = "xhci-pci", test))]
            Self::EndpointGate { info, .. } => info,
        }
    }

    fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        match self {
            Self::PlatformRoute(_) => Ok(()),
            #[cfg(any(feature = "xhci-pci", test))]
            Self::EndpointGate { lease, .. } => lease.enable_binding_irq(),
        }
    }

    fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        match self {
            Self::PlatformRoute(_) => Ok(()),
            #[cfg(any(feature = "xhci-pci", test))]
            Self::EndpointGate { lease, .. } => lease.disable_binding_irq(),
        }
    }
}

trait UsbDeviceIrqControl {
    fn enable_device_irq(&mut self) -> crab_usb::err::Result;
    fn disable_device_irq(&mut self) -> crab_usb::err::Result;
}

impl UsbDeviceIrqControl for USBHost {
    fn enable_device_irq(&mut self) -> crab_usb::err::Result {
        self.enable_irq()
    }

    fn disable_device_irq(&mut self) -> crab_usb::err::Result {
        self.disable_irq()
    }
}

fn enable_usb_irq_transaction<D: UsbDeviceIrqControl>(
    device: &mut D,
    binding: &UsbIrqBindingOwner,
) -> Result<(), UsbIrqLifecycleError> {
    if let Err(error) = device.enable_device_irq() {
        let device_rollback_error = device.disable_device_irq().err().map(Box::new);
        let binding_rollback_error = binding.disable_binding_irq().err().map(Box::new);
        return Err(UsbIrqLifecycleError::DeviceEnable {
            error: Box::new(error),
            device_rollback_error,
            binding_rollback_error,
        });
    }

    if let Err(error) = binding.enable_binding_irq() {
        let device_rollback_error = device.disable_device_irq().err().map(Box::new);
        let binding_rollback_error = binding.disable_binding_irq().err().map(Box::new);
        return Err(UsbIrqLifecycleError::BindingEnable {
            error: Box::new(error),
            device_rollback_error,
            binding_rollback_error,
        });
    }

    Ok(())
}

fn disable_usb_irq_transaction<D: UsbDeviceIrqControl>(
    device: &mut D,
    binding: &UsbIrqBindingOwner,
) -> Result<(), UsbIrqLifecycleError> {
    let device_error = device.disable_device_irq().err().map(Box::new);
    let binding_error = binding.disable_binding_irq().err().map(Box::new);
    if device_error.is_none() && binding_error.is_none() {
        Ok(())
    } else {
        Err(UsbIrqLifecycleError::Disable {
            device_error,
            binding_error,
        })
    }
}

impl PlatformUsbHost {
    fn new(name: &'static str, host: USBHost, info: BindingInfo) -> Self {
        Self::new_with_binding_owner(
            name,
            host,
            UsbIrqBindingOwner::platform_route(info),
            Speed::SuperSpeedPlus,
        )
    }

    fn new_with_root_hub_speed(
        name: &'static str,
        host: USBHost,
        info: BindingInfo,
        root_hub_speed: Speed,
    ) -> Self {
        Self::new_with_binding_owner(
            name,
            host,
            UsbIrqBindingOwner::platform_route(info),
            root_hub_speed,
        )
    }

    #[cfg(any(feature = "xhci-pci", test))]
    fn new_with_irq_lease<L: IrqBindingLease>(name: &'static str, host: USBHost, lease: L) -> Self {
        Self::new_with_binding_owner(
            name,
            host,
            UsbIrqBindingOwner::endpoint_gate(lease),
            Speed::SuperSpeedPlus,
        )
    }

    fn new_with_binding_owner(
        name: &'static str,
        host: USBHost,
        irq_binding: UsbIrqBindingOwner,
        root_hub_speed: Speed,
    ) -> Self {
        Self {
            name,
            irq_binding,
            host,
            root_hub_speed,
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
        self.irq_binding.binding_info().irq_num()
    }

    pub fn irq(&self) -> Option<&BindingIrq> {
        self.irq_binding.binding_info().irq()
    }

    pub fn irq_cloned(&self) -> Option<BindingIrq> {
        self.irq_binding.binding_info().irq_cloned()
    }

    pub fn binding_info(&self) -> &BindingInfo {
        self.irq_binding.binding_info()
    }

    pub fn root_hub_speed(&self) -> Speed {
        self.root_hub_speed
    }

    pub fn enable_irq(&mut self) -> Result<(), UsbIrqLifecycleError> {
        enable_usb_irq_transaction(&mut self.host, &self.irq_binding)
    }

    pub fn disable_irq(&mut self) -> Result<(), UsbIrqLifecycleError> {
        disable_usb_irq_transaction(&mut self.host, &self.irq_binding)
    }

    pub fn take_irq_handler(&mut self) -> Option<(usize, UsbHostIrqHandler)> {
        let irq = self.irq_binding.binding_info().irq_num()?;
        let handler = self.take_event_handler()?;
        Some((irq, handler))
    }

    pub fn take_binding_irq_handler(&mut self) -> Option<(BindingIrq, UsbHostIrqHandler)> {
        let irq = self.irq_binding.binding_info().irq_cloned()?;
        let handler = self.take_event_handler()?;
        Some((irq, handler))
    }

    pub fn take_event_handler(&mut self) -> Option<UsbHostIrqHandler> {
        if self.irq_handler_taken {
            return None;
        }

        self.irq_handler_taken = true;
        let handler = UsbHostIrqHandler::new(self.host.create_event_handler());
        Some(handler)
    }
}

impl DriverGeneric for PlatformUsbHost {
    fn name(&self) -> &str {
        self.name
    }
}

impl BoundDevice for PlatformUsbHost {
    fn binding_info(&self) -> &BindingInfo {
        self.irq_binding.binding_info()
    }
}

pub struct UsbHostIrqHandler {
    handler: EventHandler,
}

impl UsbHostIrqHandler {
    fn new(handler: EventHandler) -> Self {
        Self { handler }
    }

    /// Captures one stable USB IRQ fact without advancing host queues.
    pub fn capture_irq(&mut self) -> IrqCapture<crab_usb::UsbIrqEvent, crab_usb::UsbIrqFault> {
        self.handler.capture_irq()
    }

    /// Advances host queues from one event already acknowledged in hard IRQ.
    pub fn service_host_events(
        &mut self,
        event: crab_usb::UsbIrqEvent,
    ) -> Result<crab_usb::Event, crab_usb::UsbIrqFault> {
        self.handler.service_host_events(event)
    }

    /// Masks the exact host interrupt source after publication cannot progress.
    pub fn contain(
        &mut self,
        cause: ContainmentCause,
    ) -> Result<MaskedSource, crab_usb::UsbIrqFault> {
        self.handler.contain_irq(cause)
    }

    /// Rearms the source generation consumed by the fixed host owner.
    pub fn rearm_sources(&mut self, source: MaskedSource) -> Result<(), crab_usb::UsbIrqFault> {
        self.handler.rearm_sources(source)
    }
}

impl IrqEndpoint for UsbHostIrqHandler {
    type Event = crab_usb::UsbIrqEvent;
    type Fault = crab_usb::UsbIrqFault;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        self.capture_irq()
    }

    fn contain(&mut self, cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        Self::contain(self, cause)
    }
}

impl IrqSourceControl for UsbHostIrqHandler {
    type Error = crab_usb::UsbIrqFault;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        self.rearm_sources(source)
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
        Ok(register_usb_host_with_info(
            self.into_platform_device(),
            name,
            host,
            info,
        ))
    }

    fn register_usb_host_with_root_hub_speed(
        self,
        name: &'static str,
        host: USBHost,
        root_hub_speed: Speed,
    ) -> Result<Option<usize>, OnProbeError> {
        let info = binding_info_from_fdt(self.info())?;
        Ok(register_usb_host_with_info_and_root_hub_speed(
            self.into_platform_device(),
            name,
            host,
            info,
            root_hub_speed,
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

fn register_usb_host_with_info(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    host: USBHost,
    info: BindingInfo,
) -> Option<usize> {
    register_bound_device(plat_dev, PlatformUsbHost::new(name, host, info))
}

#[cfg(feature = "xhci-pci")]
fn register_usb_host_with_irq_lease<L: IrqBindingLease>(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    host: USBHost,
    lease: L,
) -> Option<usize> {
    register_bound_device(
        plat_dev,
        PlatformUsbHost::new_with_irq_lease(name, host, lease),
    )
}

fn register_usb_host_with_info_and_root_hub_speed(
    plat_dev: rdrive::PlatformDevice,
    name: &'static str,
    host: USBHost,
    info: BindingInfo,
    root_hub_speed: Speed,
) -> Option<usize> {
    register_bound_device(
        plat_dev,
        PlatformUsbHost::new_with_root_hub_speed(name, host, info, root_hub_speed),
    )
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
    use alloc::{boxed::Box, sync::Arc, vec, vec::Vec};
    use core::{alloc::Layout, num::NonZeroUsize, ptr::NonNull};

    use crab_usb::{Dwc2HostParams, Dwc2NewParams, USBHost, usb_if::Speed};
    use dma_api::{DmaAllocHandle, DmaConstraints, DmaDirection, DmaError, DmaMapHandle, DmaOp};
    use spin::Mutex;

    use super::*;
    use crate::{IrqBindingFailure, IrqBindingFault, IrqBindingOperation, IrqBindingStage};

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

        unsafe fn dealloc_coherent(&self, _handle: DmaAllocHandle) {}

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
        let mut host = PlatformUsbHost::new_with_root_hub_speed(
            "test-usb",
            test_usb_host(),
            info,
            Speed::High,
        );

        assert_eq!(host.irq_num(), None);
        let (actual, _handler) = host
            .take_binding_irq_handler()
            .expect("binding IRQ handler should be available");
        assert_eq!(actual, binding);
        assert!(host.take_binding_irq_handler().is_none());
    }

    struct TestDeviceIrq {
        transitions: Arc<Mutex<Vec<&'static str>>>,
        fail_enable: bool,
        fail_disable: bool,
    }

    impl UsbDeviceIrqControl for TestDeviceIrq {
        fn enable_device_irq(&mut self) -> crab_usb::err::Result {
            self.transitions.lock().push("device-enable");
            if self.fail_enable {
                Err(crab_usb::err::USBError::NotSupported)
            } else {
                Ok(())
            }
        }

        fn disable_device_irq(&mut self) -> crab_usb::err::Result {
            self.transitions.lock().push("device-disable");
            if self.fail_disable {
                Err(crab_usb::err::USBError::NotSupported)
            } else {
                Ok(())
            }
        }
    }

    struct TestBindingLease {
        info: BindingInfo,
        transitions: Arc<Mutex<Vec<&'static str>>>,
        fail_enable: bool,
        fail_disable: bool,
    }

    impl IrqBindingLease for TestBindingLease {
        fn binding_info(&self) -> BindingInfo {
            self.info.clone()
        }

        fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
            self.transitions.lock().push("binding-enable");
            if self.fail_enable {
                Err(test_binding_error(IrqBindingOperation::Enable))
            } else {
                Ok(())
            }
        }

        fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
            self.transitions.lock().push("binding-disable");
            if self.fail_disable {
                Err(test_binding_error(IrqBindingOperation::Disable))
            } else {
                Ok(())
            }
        }
    }

    fn test_binding_error(operation: IrqBindingOperation) -> IrqBindingError {
        IrqBindingError::new(
            operation,
            IrqBindingFault::new(
                IrqBindingStage::ProviderVector,
                None,
                IrqBindingFailure::InvalidVector,
            ),
        )
    }

    fn test_endpoint_owner(
        transitions: Arc<Mutex<Vec<&'static str>>>,
        fail_enable: bool,
        fail_disable: bool,
    ) -> UsbIrqBindingOwner {
        let binding =
            BindingIrq::fdt_interrupt_with_controller(rdrive::DeviceId::new(), [0, 42, 4]);
        UsbIrqBindingOwner::endpoint_gate(TestBindingLease {
            info: BindingInfo::with_binding_irq(Some(binding)),
            transitions,
            fail_enable,
            fail_disable,
        })
    }

    #[test]
    fn usb_irq_enable_publishes_device_before_outer_gate() {
        let transitions = Arc::new(Mutex::new(Vec::new()));
        let binding = test_endpoint_owner(transitions.clone(), false, false);
        let mut device = TestDeviceIrq {
            transitions: transitions.clone(),
            fail_enable: false,
            fail_disable: false,
        };

        enable_usb_irq_transaction(&mut device, &binding).unwrap();

        assert_eq!(*transitions.lock(), ["device-enable", "binding-enable"]);
        assert!(binding.binding_info().irq().is_some());
    }

    #[test]
    fn usb_irq_binding_enable_failure_contains_both_layers() {
        let transitions = Arc::new(Mutex::new(Vec::new()));
        let binding = test_endpoint_owner(transitions.clone(), true, false);
        let mut device = TestDeviceIrq {
            transitions: transitions.clone(),
            fail_enable: false,
            fail_disable: false,
        };

        let error = enable_usb_irq_transaction(&mut device, &binding).unwrap_err();

        assert!(matches!(error, UsbIrqLifecycleError::BindingEnable { .. }));
        assert_eq!(
            *transitions.lock(),
            [
                "device-enable",
                "binding-enable",
                "device-disable",
                "binding-disable"
            ]
        );
    }

    #[test]
    fn usb_irq_device_enable_failure_keeps_outer_gate_closed() {
        let transitions = Arc::new(Mutex::new(Vec::new()));
        let binding = test_endpoint_owner(transitions.clone(), false, false);
        let mut device = TestDeviceIrq {
            transitions: transitions.clone(),
            fail_enable: true,
            fail_disable: false,
        };

        let error = enable_usb_irq_transaction(&mut device, &binding).unwrap_err();

        assert!(matches!(error, UsbIrqLifecycleError::DeviceEnable { .. }));
        assert_eq!(
            *transitions.lock(),
            ["device-enable", "device-disable", "binding-disable"]
        );
    }

    #[test]
    fn usb_irq_disable_attempts_outer_gate_after_device_failure() {
        let transitions = Arc::new(Mutex::new(Vec::new()));
        let binding = test_endpoint_owner(transitions.clone(), false, false);
        let mut device = TestDeviceIrq {
            transitions: transitions.clone(),
            fail_enable: false,
            fail_disable: true,
        };

        let error = disable_usb_irq_transaction(&mut device, &binding).unwrap_err();

        assert!(matches!(
            error,
            UsbIrqLifecycleError::Disable {
                device_error: Some(_),
                binding_error: None,
            }
        ));
        assert_eq!(*transitions.lock(), ["device-disable", "binding-disable"]);
    }
}
