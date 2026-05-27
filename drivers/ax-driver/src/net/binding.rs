extern crate alloc;

use alloc::boxed::Box;

use rd_net::{Interface, NetError};
use rdrive::{Device, DriverGeneric};

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

#[cfg(any(
    feature = "intel-net",
    feature = "ixgbe",
    feature = "realtek-rtl8125",
    feature = "virtio-net"
))]
pub fn pci_legacy_irq(endpoint: &rdrive::probe::pci::EndpointRc) -> Option<usize> {
    crate::pci::endpoint_legacy_irq(endpoint)
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
