use alloc::vec::Vec;
use core::hint::spin_loop;

use crate::{
    Endpoint, PciAddress, PciConfigSpace, PciHeaderBase, PciPciBridge, chip::PcieController,
};

const MAX_DEVICE: u8 = 31;
const MAX_FUNCTION: u8 = 7;

pub fn enumerate_by_controller<'a>(
    controller: &'a mut PcieController,
    range: Option<core::ops::Range<usize>>,
) -> impl Iterator<Item = Endpoint> + 'a {
    enumerate_by_controller_with_info(controller, range).map(|item| item.endpoint)
}

pub fn enumerate_by_controller_with_info<'a>(
    controller: &'a mut PcieController,
    range: Option<core::ops::Range<usize>>,
) -> impl Iterator<Item = EnumeratedEndpoint> + 'a {
    let range = range.unwrap_or(0..0x100);

    PciIterator {
        root: controller,
        segment: 0,
        bus_max: (range.end - 1) as _,
        function: 0,
        is_mulitple_function: false,
        is_finish: false,
        stack: alloc::vec![Bridge::root(range.start as _)],
    }
}

#[derive(Debug)]
pub struct EnumeratedEndpoint {
    pub endpoint: Endpoint,
    pub intx_route: Option<PciIntxRoute>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PciIntxRoute {
    pub root_device: u8,
    pub root_function: u8,
    pub root_pin: u8,
}

pub(crate) struct PciIterator<'a> {
    root: &'a mut PcieController,
    segment: u16,
    stack: Vec<Bridge>,
    bus_max: u8,
    function: u8,
    is_mulitple_function: bool,
    is_finish: bool,
}

impl<'a> Iterator for PciIterator<'a> {
    type Item = EnumeratedEndpoint;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.is_finish {
            if let Some(value) = self.get_current_valid() {
                match value {
                    PciConfigSpace::PciPciBridge(pci_pci_bridge) => {
                        self.next(Some(pci_pci_bridge));
                    }
                    PciConfigSpace::Endpoint(ep) => {
                        let intx_route = self.intx_route_for_endpoint(&ep);
                        let item = EnumeratedEndpoint {
                            endpoint: ep,
                            intx_route,
                        };
                        self.next(None);
                        return Some(item);
                    }
                    PciConfigSpace::CardBusBridge(_) | PciConfigSpace::Unknown(_) => {
                        // Not handled for iteration; skip
                        self.next(None);
                    }
                }
            } else {
                self.next(None);
            }
        }
        None
    }
}

impl<'a> PciIterator<'a> {
    fn get_current_valid(&mut self) -> Option<PciConfigSpace> {
        let address = self.address();
        let header_base = PciHeaderBase::new(self.root, address)?;
        self.is_mulitple_function = header_base.has_multiple_functions();

        match header_base.header_type() {
            pci_types::HeaderType::Endpoint => {
                let bl = self.root.bar_allocator.as_mut();
                let ep = Endpoint::new(header_base, bl);
                Some(PciConfigSpace::Endpoint(ep))
            }
            pci_types::HeaderType::PciPciBridge => {
                let mut bridge = PciPciBridge::new(header_base);
                let primary_bus = address.bus();
                let secondary_bus;

                if let Some(parent) = self.stack.last_mut() {
                    if parent.bridge.subordinate_bus_number() == self.bus_max {
                        return None;
                    }

                    secondary_bus = parent.bridge.subordinate_bus_number() + 1;
                } else {
                    panic!("no parent");
                }
                let subordinate_bus = secondary_bus;
                bridge.update_bus_number(|mut bus| {
                    bus.primary = primary_bus;
                    bus.secondary = secondary_bus;
                    bus.subordinate = subordinate_bus;
                    bus
                });

                Some(PciConfigSpace::PciPciBridge(bridge))
            }
            pci_types::HeaderType::CardBusBridge => todo!(),
            pci_types::HeaderType::Unknown(_) => todo!(),
            _ => unreachable!(),
        }
    }

    fn address(&self) -> PciAddress {
        let parent = self.stack.last().unwrap();
        let bus = parent.bridge.secondary_bus_number();
        let device = parent.device;

        PciAddress::new(self.segment, bus, device, self.function)
    }

    fn intx_route_for_endpoint(&self, endpoint: &Endpoint) -> Option<PciIntxRoute> {
        root_intx_route(
            endpoint.address(),
            endpoint.interrupt_pin(),
            self.stack.iter().skip(1).map(|bridge| BridgeAddress {
                device: bridge.parent_device,
                function: bridge.parent_function,
            }),
        )
    }

    /// 若进位返回true
    fn is_next_function_max(&mut self) -> bool {
        if self.is_mulitple_function {
            if self.function == MAX_FUNCTION {
                self.function = 0;
                true
            } else {
                self.function += 1;
                false
            }
        } else {
            self.function = 0;
            true
        }
    }

    /// 若进位返回true
    fn next_device_not_ok(&mut self) -> bool {
        if let Some(parent) = self.stack.last_mut() {
            if parent.device == MAX_DEVICE {
                if let Some(parent) = self.stack.pop() {
                    self.is_finish = parent.bridge.subordinate_bus_number() == self.bus_max;

                    // parent.header.sync_bus_number(&self.root);
                    self.function = 0;
                    return true;
                } else {
                    self.is_finish = true;
                }
            } else {
                parent.device += 1;
            }
        } else {
            self.is_finish = true;
        }

        false
    }

    fn next(&mut self, current_bridge: Option<PciPciBridge>) {
        if let Some(bridge) = current_bridge {
            for parent in &mut self.stack {
                // parent.header.subordinate_bus += 1;

                parent.bridge.update_bus_number(|mut bus| {
                    bus.subordinate += 1;
                    bus
                });
            }

            let address = bridge.address();
            self.stack.push(Bridge {
                bridge,
                device: 0,
                parent_device: address.device(),
                parent_function: address.function(),
            });

            self.function = 0;
            return;
        }

        if self.is_next_function_max() {
            while self.next_device_not_ok() {
                spin_loop();
            }
        }
    }
}

struct Bridge {
    bridge: PciPciBridge,
    device: u8,
    parent_device: u8,
    parent_function: u8,
}

impl Bridge {
    fn root(bus_start: u8) -> Self {
        Self {
            bridge: PciPciBridge::root(),
            device: bus_start,
            parent_device: 0,
            parent_function: 0,
        }
    }
}

const fn swizzle_interrupt_pin(interrupt_pin: u8, device: u8) -> Option<u8> {
    if interrupt_pin == 0 || interrupt_pin > 4 {
        return None;
    }
    Some(((interrupt_pin - 1 + (device & 0x3)) % 4) + 1)
}

#[derive(Clone, Copy)]
struct BridgeAddress {
    device: u8,
    function: u8,
}

fn root_intx_route(
    endpoint: PciAddress,
    interrupt_pin: u8,
    bridges_from_root: impl DoubleEndedIterator<Item = BridgeAddress>,
) -> Option<PciIntxRoute> {
    let mut pin = interrupt_pin;
    if !(1..=4).contains(&pin) {
        return None;
    }

    let mut device = endpoint.device();
    let mut function = endpoint.function();
    for bridge in bridges_from_root.rev() {
        pin = swizzle_interrupt_pin(pin, device)?;
        device = bridge.device;
        function = bridge.function;
    }

    Some(PciIntxRoute {
        root_device: device,
        root_function: function,
        root_pin: pin,
    })
}

#[cfg(test)]
mod tests {
    use super::{BridgeAddress, PciIntxRoute, root_intx_route, swizzle_interrupt_pin};
    use crate::PciAddress;

    #[test]
    fn intx_swizzle_uses_linux_slot_rotation() {
        assert_eq!(swizzle_interrupt_pin(1, 0), Some(1));
        assert_eq!(swizzle_interrupt_pin(1, 1), Some(2));
        assert_eq!(swizzle_interrupt_pin(1, 2), Some(3));
        assert_eq!(swizzle_interrupt_pin(1, 3), Some(4));
        assert_eq!(swizzle_interrupt_pin(4, 1), Some(1));
    }

    #[test]
    fn intx_swizzle_rejects_absent_or_invalid_pins() {
        assert_eq!(swizzle_interrupt_pin(0, 1), None);
        assert_eq!(swizzle_interrupt_pin(5, 1), None);
    }

    #[test]
    fn root_endpoint_uses_its_own_device_function_and_pin() {
        let route = root_intx_route(PciAddress::new(0, 0, 5, 2), 3, [].into_iter());

        assert_eq!(
            route,
            Some(PciIntxRoute {
                root_device: 5,
                root_function: 2,
                root_pin: 3,
            })
        );
    }

    #[test]
    fn bridge_endpoint_uses_parent_bridge_slot_and_swizzled_pin() {
        let route = root_intx_route(
            PciAddress::new(0, 1, 3, 0),
            1,
            [BridgeAddress {
                device: 2,
                function: 0,
            }]
            .into_iter(),
        );

        assert_eq!(
            route,
            Some(PciIntxRoute {
                root_device: 2,
                root_function: 0,
                root_pin: 4,
            })
        );
    }

    #[test]
    fn nested_bridge_endpoint_swizzles_at_each_level() {
        let route = root_intx_route(
            PciAddress::new(0, 2, 1, 0),
            2,
            [
                BridgeAddress {
                    device: 4,
                    function: 1,
                },
                BridgeAddress {
                    device: 3,
                    function: 0,
                },
            ]
            .into_iter(),
        );

        assert_eq!(
            route,
            Some(PciIntxRoute {
                root_device: 4,
                root_function: 1,
                root_pin: 2,
            })
        );
    }
}

#[cfg(axtest)]
pub(crate) fn pci_constants_hold_for_test() -> bool {
    // Test PCI constants
    assert_eq!(MAX_DEVICE, 31);
    assert_eq!(MAX_FUNCTION, 7);
    
    true
}
