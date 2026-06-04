extern crate alloc;

use alloc::boxed::Box;

use rd_net::{Interface, NetError};
use rdrive::{Device, DriverGeneric, probe::pci::EndpointRc};

pub struct PlatformNetDevice {
    name: &'static str,
    irq_num: Option<usize>,
    net: Option<rd_net::Net>,
}

impl PlatformNetDevice {
    fn new(name: &'static str, net: rd_net::Net, irq_num: Option<usize>) -> Self {
        Self {
            name,
            irq_num,
            net: Some(net),
        }
    }

    pub fn take_net(&mut self) -> Option<(rd_net::Net, &'static str, Option<usize>)> {
        Some((self.net.take()?, self.name, self.irq_num))
    }
}

pub fn take_rd_net_device(
    device: Device<PlatformNetDevice>,
) -> Result<(rd_net::Net, &'static str, Option<usize>), NetError> {
    let mut dev = device
        .lock()
        .map_err(|_| NetError::Other(Box::new(rd_net::KError::Unknown("device locked"))))?;
    dev.take_net()
        .ok_or_else(|| NetError::Other(Box::new(rd_net::KError::Unknown("device already taken"))))
}

impl DriverGeneric for PlatformNetDevice {
    fn name(&self) -> &str {
        self.name
    }
}

pub fn pci_legacy_irq(endpoint: &EndpointRc) -> Option<usize> {
    pci_irq_candidates(
        endpoint.address(),
        endpoint.interrupt_pin(),
        endpoint.interrupt_line(),
        || {
            #[cfg(all(
                plat_dyn,
                any(target_os = "none", arceos_std),
                any(
                    feature = "intel-net",
                    feature = "ixgbe",
                    feature = "realtek-rtl8125",
                    feature = "virtio-net",
                    feature = "xhci-pci",
                )
            ))]
            {
                let interrupt_pin = endpoint.interrupt_pin();
                if interrupt_pin != 0 {
                    match crate::pci::acpi_irq_for_endpoint(endpoint.address(), interrupt_pin) {
                        Ok(Some(irq)) => return Some(irq),
                        Ok(None) => {}
                        Err(err) => log::warn!(
                            "failed to resolve ACPI IRQ for net endpoint {}: {err}",
                            endpoint.address()
                        ),
                    }
                }
            }
            None
        },
        || {
            #[cfg(all(
                plat_dyn,
                any(target_os = "none", arceos_std),
                any(
                    feature = "intel-net",
                    feature = "ixgbe",
                    feature = "realtek-rtl8125",
                    feature = "virtio-net",
                    feature = "xhci-pci",
                )
            ))]
            {
                let interrupt_pin = endpoint.interrupt_pin();
                if interrupt_pin != 0 {
                    match crate::pci::fdt_irq_for_endpoint(endpoint.address(), interrupt_pin) {
                        Ok(Some(irq)) => return Some(irq),
                        Ok(None) => {}
                        Err(err) => log::warn!(
                            "failed to resolve FDT IRQ for net endpoint {}: {err}",
                            endpoint.address()
                        ),
                    }
                }
            }
            None
        },
    )
}

fn pci_irq_candidates(
    address: rdrive::probe::pci::PciAddress,
    interrupt_pin: u8,
    interrupt_line: u8,
    acpi: impl FnOnce() -> Option<usize>,
    fdt: impl FnOnce() -> Option<usize>,
) -> Option<usize> {
    if let Some(irq) = acpi() {
        return Some(irq);
    }

    if let Some(irq) = fdt() {
        return Some(irq);
    }

    if let Some(irq) = crate::pci::legacy_irq_for_endpoint(address, interrupt_pin) {
        return Some(irq);
    }

    if interrupt_line == 0 || interrupt_line == u8::MAX {
        return None;
    }
    Some(crate::pci::legacy_line_to_irq(interrupt_line))
}

#[cfg(test)]
mod tests {
    use rdrive::probe::pci::PciAddress;

    use super::pci_irq_candidates;

    #[test]
    fn pci_irq_resolution_prefers_acpi_then_fdt_then_fallback_line() {
        let address = PciAddress::new(0, 0, 3, 0);

        assert_eq!(
            pci_irq_candidates(address, 1, 9, || Some(0x31), || Some(0x40)),
            Some(0x31)
        );
        assert_eq!(
            pci_irq_candidates(address, 1, 9, || None, || Some(0x40)),
            Some(0x40)
        );
        let fallback_irq = pci_irq_candidates(address, 1, 9, || None, || None);
        if cfg!(all(target_arch = "x86_64", plat_dyn)) {
            assert_eq!(fallback_irq, Some(0x39));
        } else if cfg!(target_arch = "x86_64") {
            assert_eq!(fallback_irq, Some(0x29));
        } else {
            assert_eq!(fallback_irq, Some(9));
        }
    }
}

pub trait PlatformDeviceNet {
    fn register_net<T>(self, name: &'static str, dev: T, irq_num: Option<usize>)
    where
        T: Interface + 'static;
}

impl PlatformDeviceNet for rdrive::PlatformDevice {
    fn register_net<T>(self, name: &'static str, dev: T, irq_num: Option<usize>)
    where
        T: Interface + 'static,
    {
        let net = rd_net::Net::new(dev, axklib::dma::op());
        self.register(PlatformNetDevice::new(name, net, irq_num));
    }
}
