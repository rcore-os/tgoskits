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

/// A registered Wi-Fi device.
///
/// Unlike [`PlatformNetDevice`] (a plain NIC), a Wi-Fi device carries two extra
/// things past the probe stage:
///
/// * the chip-facing control plane ([`wifi_host::WifiDriver`]) for STA/SoftAP
///   control (connect / start_ap / MAC / RX-wake), and
/// * an [`ApConfig`] describing the link policy the board wants applied once the
///   network service is up.
///
/// The data plane is the same `rd_net::Net` every NIC uses. Keeping the control
/// handle and policy *with the device* (rather than in the protocol stack) is
/// what lets the network stack stay Wi-Fi-agnostic.
#[cfg(feature = "aic8800-wifi")]
pub struct PlatformWifiDevice {
    name: &'static str,
    net: Option<rd_net::Net>,
    wifi: Option<Box<dyn wifi_host::WifiDriver>>,
    ap: ApConfig,
}

/// Link policy for a Wi-Fi device, produced by the board/probe layer and applied
/// by the runtime when it brings the network service up. The protocol stack only
/// consumes the generic fields; it has no notion of "Wi-Fi" or "SoftAP".
#[cfg(feature = "aic8800-wifi")]
#[derive(Clone, Copy)]
pub struct ApConfig {
    /// SoftAP gateway / this interface's static address.
    pub server_ip: [u8; 4],
    /// Address handed out to the single DHCP client.
    pub client_ip: [u8; 4],
    pub prefix_len: u8,
}

/// The parts taken out of a [`PlatformWifiDevice`]: data plane, control plane,
/// device name, and link policy.
#[cfg(feature = "aic8800-wifi")]
pub type WifiDeviceParts = (
    rd_net::Net,
    Box<dyn wifi_host::WifiDriver>,
    &'static str,
    ApConfig,
);

#[cfg(feature = "aic8800-wifi")]
impl PlatformWifiDevice {
    pub fn new(
        name: &'static str,
        net: rd_net::Net,
        wifi: Box<dyn wifi_host::WifiDriver>,
        ap: ApConfig,
    ) -> Self {
        Self {
            name,
            net: Some(net),
            wifi: Some(wifi),
            ap,
        }
    }

    /// Takes the data plane, control plane, name and AP policy. Returns `None`
    /// if already taken.
    pub fn take(&mut self) -> Option<WifiDeviceParts> {
        Some((self.net.take()?, self.wifi.take()?, self.name, self.ap))
    }
}

#[cfg(feature = "aic8800-wifi")]
impl DriverGeneric for PlatformWifiDevice {
    fn name(&self) -> &str {
        self.name
    }
}

/// Takes the parts of a registered Wi-Fi device out of its rdrive slot.
#[cfg(feature = "aic8800-wifi")]
pub fn take_wifi_device(device: Device<PlatformWifiDevice>) -> Result<WifiDeviceParts, NetError> {
    let mut dev = device
        .lock()
        .map_err(|_| NetError::Other(Box::new(rd_net::KError::Unknown("device locked"))))?;
    dev.take()
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
            #[cfg(pci_dyn_acpi_intx_route)]
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
            #[cfg(pci_dyn_intx_route)]
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
