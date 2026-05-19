extern crate alloc;

use alloc::boxed::Box;

use rd_net::{Interface, NetError};
use rdrive::{Device, DriverGeneric};

use super::DmaImpl;

#[cfg(feature = "intel-net")]
mod intel;
#[cfg(feature = "realtek-rtl8125")]
mod realtek;

pub struct PlatformNetDevice {
    name: &'static str,
    net: Option<rd_net::Net>,
    irq_num: Option<usize>,
}

impl PlatformNetDevice {
    fn new(name: &'static str, net: rd_net::Net, irq_num: Option<usize>) -> Self {
        Self {
            name,
            net: Some(net),
            irq_num,
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

pub(super) fn pci_legacy_irq_for_address(address: rdrive::probe::pci::PciAddress) -> usize {
    if let Some(irq) = super::pci::legacy_irq_for_address(address) {
        return irq;
    }

    const PCI_IRQ_BASE: usize = if cfg!(target_arch = "x86_64") || cfg!(target_arch = "riscv64") {
        0x20
    } else if cfg!(target_arch = "loongarch64") {
        0x10
    } else if cfg!(target_arch = "aarch64") {
        0x23
    } else {
        0
    };

    PCI_IRQ_BASE + (usize::from(address.device()) & 3)
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
        let net = rd_net::Net::new(dev, &DmaImpl);
        self.register(PlatformNetDevice::new(name, net, irq_num));
    }
}
