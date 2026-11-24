use pcie::{Controller, PciAddress, PcieController, enumerate_by_controller};
use rdif_pcie::DriverGeneric;

/// Configuration-space model with two root ports and one endpoint per port.
/// Downstream reads follow the bus numbers programmed into the bridges.
#[derive(Default)]
struct TwoRootPorts {
    buses: [u32; 2],
}

impl DriverGeneric for TwoRootPorts {
    fn name(&self) -> &str {
        "two-root-port configuration model"
    }
}

impl Controller for TwoRootPorts {
    fn read(&mut self, address: PciAddress, offset: u16) -> u32 {
        if address.function() != 0 {
            return u32::MAX;
        }
        if address.bus() == 0 && (1..=2).contains(&address.device()) {
            return match offset {
                0 => 0x0001_1234,
                8 => 0x0604_0000,
                0x0c => 1 << 16,
                0x18 => self.buses[usize::from(address.device() - 1)],
                _ => 0,
            };
        }
        if address.bus() != 0
            && address.device() == 0
            && let Some(port) = self
                .buses
                .iter()
                .position(|buses| ((buses >> 8) & 0xff) == u32::from(address.bus()))
        {
            return match offset {
                0 => ((0x1000 + port as u32) << 16) | 0x1234,
                8 => 0x0200_0000,
                _ => 0,
            };
        }
        u32::MAX
    }

    fn write(&mut self, address: PciAddress, offset: u16, value: u32) {
        if address.bus() == 0
            && (1..=2).contains(&address.device())
            && address.function() == 0
            && offset == 0x18
        {
            self.buses[usize::from(address.device() - 1)] = value;
        }
    }
}

#[test]
fn enumeration_reaches_each_root_ports_own_endpoint() {
    let mut controller = PcieController::new(TwoRootPorts::default());
    let endpoints: alloc::vec::Vec<_> = enumerate_by_controller(&mut controller, None)
        .map(|endpoint| (endpoint.address().bus(), endpoint.device_id()))
        .collect();
    assert_eq!(endpoints, [(1, 0x1000), (2, 0x1001)]);
}
