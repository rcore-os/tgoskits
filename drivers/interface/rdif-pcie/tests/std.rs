use pci_types::PciAddress;
use rdif_pcie::{
    DriverGeneric, Interface, PciMem32, PciMem64, PcieController, SimpleBarAllocator,
    addr_alloc::{AllocPolicy, Constraint, DEFAULT_CONSTRAINT_ALIGN, Error, RangeInclusive},
};

struct MockPcie;

impl DriverGeneric for MockPcie {
    fn name(&self) -> &str {
        "mock-pcie"
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl Interface for MockPcie {
    fn read(&mut self, _address: PciAddress, _offset: u16) -> u32 {
        panic!("BAR allocation must not read PCI configuration space");
    }

    fn write(&mut self, _address: PciAddress, _offset: u16, _value: u32) {
        panic!("BAR allocation must not write PCI configuration space");
    }
}

#[test]
fn rdif_pcie_controller_initializes_bar_windows() {
    let mut controller = PcieController::new(MockPcie);

    controller.set_mem32(
        PciMem32 {
            address: 0x1000_0000,
            size: 0x2000,
        },
        false,
    );
    controller.set_mem64(
        PciMem64 {
            address: 0x8_0000_0000,
            size: 0x2000,
        },
        false,
    );
    let allocator = controller.bar_allocator.as_mut().unwrap();
    assert_eq!(allocator.alloc_memory32(0x1000, false), Some(0x1000_0000));
    assert_eq!(allocator.alloc_memory64(0x1000, false), Some(0x8_0000_0000));
}

#[test]
fn rdif_pcie_range_and_constraint_validation_rules_hold() {
    assert_eq!(
        RangeInclusive::new(2, 1).unwrap_err(),
        Error::InvalidRange(2, 1)
    );
    assert_eq!(
        RangeInclusive::new(0, u64::MAX).unwrap_err(),
        Error::InvalidRange(0, u64::MAX)
    );

    let range = RangeInclusive::new(2, 6).unwrap();
    assert!(range.contains(&RangeInclusive::new(3, 5).unwrap()));
    assert!(!range.contains(&RangeInclusive::new(1, 5).unwrap()));
    assert!(range.overlaps(&RangeInclusive::new(6, 8).unwrap()));
    assert!(!range.overlaps(&RangeInclusive::new(7, 8).unwrap()));

    assert_eq!(
        Constraint::new(0, DEFAULT_CONSTRAINT_ALIGN, AllocPolicy::FirstMatch).unwrap_err(),
        Error::InvalidSize(0)
    );
    assert_eq!(
        Constraint::new(0x100, 3, AllocPolicy::FirstMatch).unwrap_err(),
        Error::InvalidAlignment
    );
    assert_eq!(
        Constraint::new(0x100, 0x100, AllocPolicy::ExactMatch(0x80)).unwrap_err(),
        Error::UnalignedAddress
    );
}

#[test]
fn rdif_pcie_bar_allocator_prefers_matching_windows_and_falls_back_to_mem32() {
    let mut allocator = SimpleBarAllocator::default();
    assert_eq!(allocator.alloc_memory32(0x1000, false), None);
    allocator
        .set_mem32(
            PciMem32 {
                address: 0x1000_0000,
                size: 0x3000,
            },
            false,
        )
        .unwrap();
    allocator
        .set_mem32(
            PciMem32 {
                address: 0x2000_0000,
                size: 0x1000,
            },
            true,
        )
        .unwrap();
    allocator
        .set_mem64(
            PciMem64 {
                address: 0x8_0000_0000,
                size: 0x2000,
            },
            false,
        )
        .unwrap();

    assert_eq!(allocator.alloc_memory64(0x1000, false), Some(0x8_0000_0000));
    assert_eq!(allocator.alloc_memory32(0x1000, false), Some(0x1000_0000));
    assert_eq!(allocator.alloc_memory64(0x1000, true), Some(0x8_0000_1000));
    assert_eq!(allocator.alloc_memory32(0x1000, true), Some(0x2000_0000));
    assert_eq!(allocator.alloc_memory32(0x4000, false), None);
}
