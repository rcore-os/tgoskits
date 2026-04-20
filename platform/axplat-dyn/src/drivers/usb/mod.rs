extern crate alloc;

use alloc::{collections::BTreeMap, vec::Vec};
use core::{
    future::{Future, IntoFuture},
    pin::pin,
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    time::Duration,
};

use crab_usb::{Device, DeviceInfo, Event, EventHandler, USBHost, err::USBError};
use fdt_edit::{Fdt, NodeType};
use rdrive::DriverGeneric;
use spin::{Mutex, Once};

mod xhci_mmio;
mod xhci_pci;

use super::DmaImpl;

impl crab_usb::KernelOp for DmaImpl {
    fn delay(&self, duration: Duration) {
        axklib::time::busy_wait(duration);
    }
}

pub(crate) static USB_KERNEL: DmaImpl = DmaImpl;
static USB_IRQ_REGISTRY: Once<Mutex<BTreeMap<usize, usize>>> = Once::new();

pub struct PlatformUsbHost {
    name: &'static str,
    irq_num: Option<usize>,
    host: USBHost,
    event_handler: EventHandler,
    initialized: bool,
}

impl PlatformUsbHost {
    fn new(
        name: &'static str,
        host: USBHost,
        event_handler: EventHandler,
        irq_num: Option<usize>,
    ) -> Self {
        Self {
            name,
            irq_num,
            host,
            event_handler,
            initialized: false,
        }
    }

    pub fn init_blocking(&mut self) -> Result<(), USBError> {
        if self.initialized {
            return Ok(());
        }

        block_on_usb(self.host.init())?;
        self.initialized = true;
        Ok(())
    }

    pub fn probe_devices_blocking(&mut self) -> Result<Vec<DeviceInfo>, USBError> {
        block_on_usb(self.host.probe_devices())
    }

    pub fn open_device_blocking(&mut self, info: &DeviceInfo) -> Result<Device, USBError> {
        block_on_usb(self.host.open_device(info))
    }

    pub fn handle_irq(&self) -> Event {
        self.event_handler.handle_event()
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
    fn register_usb_host(self, name: &'static str, host: USBHost, irq_num: Option<usize>);
}

impl PlatformDeviceUsbHost for rdrive::PlatformDevice {
    fn register_usb_host(self, name: &'static str, mut host: USBHost, irq_num: Option<usize>) {
        let device_id = self.descriptor.device_id();
        let event_handler = host.create_event_handler();
        self.register(PlatformUsbHost::new(name, host, event_handler, irq_num));

        #[cfg(feature = "irq")]
        if let Some(irq_num) = irq_num {
            let device = rdrive::get::<PlatformUsbHost>(device_id)
                .expect("registered USB host should be retrievable from rdrive");
            let host_ptr = unsafe { device.force_use() };
            register_usb_irq_handler(irq_num, host_ptr);
        }
    }
}

fn align_up_4k(size: usize) -> usize {
    const MASK: usize = 0xfff;
    (size + MASK) & !MASK
}

fn decode_fdt_irq(interrupts: &[rdrive::probe::fdt::InterruptRef]) -> Option<usize> {
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

pub(super) fn resolve_pci_irq_from_fdt(
    endpoint: &rdrive::probe::pci::EndpointRc,
) -> Result<usize, USBError> {
    let fdt_addr = somehal::fdt_addr()
        .unwrap_or_else(|| panic!("PCI USB IRQ mapping requires FDT; ACPI is not supported"));
    let fdt = unsafe { Fdt::from_ptr(fdt_addr) }
        .map_err(|err| USBError::Other(anyhow::anyhow!("failed to parse live FDT: {err:?}")))?;

    let bus = endpoint.address().bus();
    let pin = endpoint.interrupt_pin();
    if pin == 0 {
        return Err(USBError::Other(anyhow::anyhow!(
            "PCI USB endpoint {} has no interrupt pin",
            endpoint.address()
        )));
    }

    let mut candidates = Vec::new();
    let mut exact_range_matches = Vec::new();
    for node in fdt.all_nodes() {
        let NodeType::Pci(pci) = node else {
            continue;
        };

        match pci.bus_range() {
            Some(range) if range.contains(&(bus as u32)) => {
                exact_range_matches.push(pci);
                candidates.push(pci);
            }
            Some(_) => {}
            None => candidates.push(pci),
        }
    }

    let pci_host = if exact_range_matches.len() == 1 {
        exact_range_matches[0]
    } else if exact_range_matches.len() > 1 {
        exact_range_matches[0]
    } else if candidates.len() == 1 {
        candidates[0]
    } else if candidates.is_empty() {
        return Err(USBError::Other(anyhow::anyhow!(
            "no PCI host node in live FDT matches USB endpoint {}",
            endpoint.address()
        )));
    } else {
        return Err(USBError::Other(anyhow::anyhow!(
            "multiple PCI host nodes in live FDT match USB endpoint {} without a unique bus-range \
             match",
            endpoint.address()
        )));
    };

    let irq = pci_host
        .child_interrupts(
            endpoint.address().bus(),
            endpoint.address().device(),
            endpoint.address().function(),
            pin,
        )
        .map_err(|err| {
            USBError::Other(anyhow::anyhow!(
                "failed to resolve PCI interrupt-map entry for USB endpoint {}: {err:?}",
                endpoint.address()
            ))
        })?;

    decode_irq_cells(&irq.irqs).ok_or_else(|| {
        USBError::Other(anyhow::anyhow!(
            "unsupported PCI interrupt specifier {:?} for USB endpoint {}",
            irq.irqs,
            endpoint.address()
        ))
    })
}

fn usb_irq_registry() -> &'static Mutex<BTreeMap<usize, usize>> {
    USB_IRQ_REGISTRY.call_once(|| Mutex::new(BTreeMap::new()))
}

#[cfg(feature = "irq")]
fn register_usb_irq_handler(irq_num: usize, host_ptr: *mut PlatformUsbHost) {
    {
        let mut registry = usb_irq_registry().lock();
        registry.insert(irq_num, host_ptr as usize);
    }

    if !ax_plat::irq::register(irq_num, usb_irq_handler) {
        warn!("failed to register USB IRQ handler for IRQ {}", irq_num);
    }
}

#[cfg(feature = "irq")]
fn usb_irq_handler() {
    let irq_num = somehal::irq::irq_handler_raw().raw();
    let host_ptr = {
        let registry = usb_irq_registry().lock();
        registry.get(&irq_num).copied()
    };

    let Some(host_ptr) = host_ptr else {
        warn!("USB IRQ {} fired without a registered USB host", irq_num);
        return;
    };

    // SAFETY: IRQ handlers must not block on the rdrive device lock.
    let host = unsafe { &*(host_ptr as *const PlatformUsbHost) };
    let event = host.handle_irq();
    trace!("USB IRQ {} handled with event {:?}", irq_num, event);
}

fn block_on_usb<F: IntoFuture>(future: F) -> F::Output {
    let mut future = pin!(future.into_future());
    let waker = noop_waker();
    let mut context = Context::from_waker(&waker);

    loop {
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => core::hint::spin_loop(),
        }
    }
}

fn noop_waker() -> Waker {
    unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &NOOP_WAKER_VTABLE)) }
}

const NOOP_WAKER_VTABLE: RawWakerVTable = RawWakerVTable::new(
    |_| RawWaker::new(core::ptr::null(), &NOOP_WAKER_VTABLE),
    |_| {},
    |_| {},
    |_| {},
);
