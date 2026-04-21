extern crate alloc;

use alloc::vec::Vec;
use core::time::Duration;

use crab_usb::{USBHost, err::USBError};
use fdt_edit::{Fdt, NodeType};
use rdrive::DriverGeneric;

mod xhci_mmio;
mod xhci_pci;

use super::DmaImpl;

pub type UsbHostDevice = rdrive::Device<PlatformUsbHost>;
pub type UsbHostDeviceGuard = rdrive::DeviceGuard<PlatformUsbHost>;

impl crab_usb::KernelOp for DmaImpl {
    fn delay(&self, duration: Duration) {
        axklib::time::busy_wait(duration);
    }
}

pub(crate) static USB_KERNEL: DmaImpl = DmaImpl;

pub struct PlatformUsbHost {
    name: &'static str,
    irq_num: Option<usize>,
    host: USBHost,
}

impl PlatformUsbHost {
    fn new(name: &'static str, host: USBHost, irq_num: Option<usize>) -> Self {
        Self {
            name,
            irq_num,
            host,
        }
    }

    pub fn host(&self) -> &USBHost {
        &self.host
    }

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
    fn register_usb_host(self, name: &'static str, host: USBHost, irq_num: Option<usize>);
}

impl PlatformDeviceUsbHost for rdrive::PlatformDevice {
    fn register_usb_host(self, name: &'static str, host: USBHost, irq_num: Option<usize>) {
        self.register(PlatformUsbHost::new(name, host, irq_num));
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

pub fn usb_host_device() -> Option<UsbHostDevice> {
    rdrive::get_one()
}
