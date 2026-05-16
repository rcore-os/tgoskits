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
    type Item = Endpoint;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.is_finish {
            if let Some(value) = self.get_current_valid() {
                match value {
                    PciConfigSpace::PciPciBridge(pci_pci_bridge) => {
                        self.next(Some(pci_pci_bridge));
                    }
                    PciConfigSpace::Endpoint(ep) => {
                        let item = ep;
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

            self.stack.push(Bridge { bridge, device: 0 });

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
}

impl Bridge {
    fn root(bus_start: u8) -> Self {
        Self {
            bridge: PciPciBridge::root(),
            device: bus_start,
        }
    }
}
