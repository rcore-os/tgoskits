use axtest::prelude::*;
use pci_types::{ConfigRegionAccess, PciAddress};

use crate::{
    DriverGeneric, Interface, PciMem32, PciMem64, PcieController, SimpleBarAllocator,
    addr_alloc::{
        AddressAllocator, AllocPolicy, Constraint, DEFAULT_CONSTRAINT_ALIGN, Error, IdAllocator,
        RangeInclusive,
    },
};

struct MockPcie {
    last_offset: u16,
    last_value: u32,
}

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
    fn read(&mut self, address: PciAddress, offset: u16) -> u32 {
        self.last_offset = offset;
        u32::from(address.bus()) << 24 | u32::from(offset)
    }

    fn write(&mut self, _address: PciAddress, offset: u16, value: u32) {
        self.last_offset = offset;
        self.last_value = value;
    }
}

#[axtest]
fn rdif_pcie_controller_delegates_driver_identity_and_config_io() {
    let controller = PcieController::new(MockPcie {
        last_offset: 0,
        last_value: 0,
    });
    let address = PciAddress::new(0, 2, 3, 0);

    ax_assert_eq!(controller.name(), "mock-pcie");

    unsafe {
        ax_assert_eq!(
            controller.read(address, 0x10),
            u32::from(address.bus()) << 24 | 0x10
        );
        controller.write(address, 0x14, 0xdead_beef);
    }
}

#[axtest]
fn rdif_pcie_config_access_is_bound_to_one_address() {
    let mut controller = PcieController::new(MockPcie {
        last_offset: 0,
        last_value: 0,
    });
    let address = PciAddress::new(0, 2, 3, 0);

    let access = controller.config_access(address);
    unsafe {
        ax_assert_eq!(
            access.read(address, 0x20),
            u32::from(address.bus()) << 24 | 0x20
        );
        access.write(address, 0x24, 0xabcd_0123);
    }
}

#[axtest]
fn rdif_pcie_controller_initializes_bar_windows() {
    let mut controller = PcieController::new(MockPcie {
        last_offset: 0,
        last_value: 0,
    });

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
    ax_assert_eq!(allocator.alloc_memory32(0x1000, false), Some(0x1000_0000));
    ax_assert_eq!(allocator.alloc_memory64(0x1000, false), Some(0x8_0000_0000));
}

#[axtest]
fn rdif_pcie_range_and_constraint_validation_rules_hold() {
    ax_assert_eq!(
        RangeInclusive::new(2, 1).unwrap_err(),
        Error::InvalidRange(2, 1)
    );
    ax_assert_eq!(
        RangeInclusive::new(0, u64::MAX).unwrap_err(),
        Error::InvalidRange(0, u64::MAX)
    );

    let range = RangeInclusive::new(2, 6).unwrap();
    ax_assert_eq!(range.start(), 2);
    ax_assert_eq!(range.end(), 6);
    ax_assert_eq!(range.len(), 5);
    ax_assert!(range.contains(&RangeInclusive::new(3, 5).unwrap()));
    ax_assert!(!range.contains(&RangeInclusive::new(1, 5).unwrap()));
    ax_assert!(range.overlaps(&RangeInclusive::new(6, 8).unwrap()));
    ax_assert!(!range.overlaps(&RangeInclusive::new(7, 8).unwrap()));

    ax_assert_eq!(
        Constraint::new(0, DEFAULT_CONSTRAINT_ALIGN, AllocPolicy::FirstMatch).unwrap_err(),
        Error::InvalidSize(0)
    );
    ax_assert_eq!(
        Constraint::new(0x100, 3, AllocPolicy::FirstMatch).unwrap_err(),
        Error::InvalidAlignment
    );
    ax_assert_eq!(
        Constraint::new(0x100, 0x100, AllocPolicy::ExactMatch(0x80)).unwrap_err(),
        Error::UnalignedAddress
    );
    let constraint = Constraint::new(0x100, 0x100, AllocPolicy::LastMatch).unwrap();
    ax_assert_eq!(constraint.size(), 0x100);
    ax_assert_eq!(constraint.align(), 0x100);
}

#[axtest]
fn rdif_pcie_id_allocator_allocates_reuses_and_reports_errors() {
    ax_assert_eq!(
        IdAllocator::new(23, 5).unwrap_err(),
        Error::InvalidRange(23, 5)
    );

    let mut ids = IdAllocator::new(5, 7).unwrap();
    ax_assert_eq!(ids.allocate_id(), Ok(5));
    ax_assert_eq!(ids.allocate_id(), Ok(6));
    ax_assert_eq!(ids.free_id(6), Ok(6));
    ax_assert_eq!(ids.allocate_id(), Ok(6));
    ax_assert_eq!(ids.allocate_id(), Ok(7));
    ax_assert_eq!(ids.allocate_id(), Err(Error::ResourceNotAvailable));
    ax_assert_eq!(ids.free_id(4), Err(Error::OutOfRange(4)));
    ax_assert_eq!(ids.free_id(6), Ok(6));
    ax_assert_eq!(ids.free_id(6), Err(Error::AlreadyReleased(6)));
    ax_assert_eq!(ids.free_id(99), Err(Error::OutOfRange(99)));

    let mut overflow = IdAllocator::new(u32::MAX - 1, u32::MAX).unwrap();
    ax_assert_eq!(overflow.allocate_id(), Ok(u32::MAX - 1));
    ax_assert_eq!(overflow.allocate_id(), Ok(u32::MAX));
    ax_assert_eq!(overflow.allocate_id(), Err(Error::Overflow));
}

#[axtest]
fn rdif_pcie_address_allocator_handles_first_last_exact_and_free_paths() {
    ax_assert_eq!(AddressAllocator::new(0x1000, 0), Err(Error::Underflow));
    ax_assert_eq!(AddressAllocator::new(u64::MAX, 0x100), Err(Error::Overflow));

    let mut pool = AddressAllocator::new(0x1000, 0x1000).unwrap();
    ax_assert_eq!(pool.base(), 0x1000);
    ax_assert_eq!(pool.end(), 0x1fff);
    ax_assert_eq!(
        pool.allocate(0x110, 0x100, AllocPolicy::FirstMatch)
            .unwrap(),
        RangeInclusive::new(0x1000, 0x110f).unwrap()
    );
    ax_assert_eq!(
        pool.allocate(0x100, 0x100, AllocPolicy::FirstMatch)
            .unwrap(),
        RangeInclusive::new(0x1200, 0x12ff).unwrap()
    );
    ax_assert_eq!(
        pool.allocate(0x200, 0x100, AllocPolicy::ExactMatch(0x1a00))
            .unwrap(),
        RangeInclusive::new(0x1a00, 0x1bff).unwrap()
    );
    ax_assert_eq!(
        pool.allocate(0x800, 0x100, AllocPolicy::ExactMatch(0x1400)),
        Err(Error::ResourceNotAvailable)
    );
    ax_assert_eq!(
        pool.free(&RangeInclusive::new(0x1200, 0x12ff).unwrap()),
        Ok(())
    );
    ax_assert_eq!(
        pool.allocate(0x100, 0x100, AllocPolicy::FirstMatch)
            .unwrap(),
        RangeInclusive::new(0x1200, 0x12ff).unwrap()
    );

    let mut reverse = AddressAllocator::new(0x1000, 0x10000).unwrap();
    ax_assert_eq!(
        reverse
            .allocate(0x110, 0x100, AllocPolicy::LastMatch)
            .unwrap(),
        RangeInclusive::new(0x10e00, 0x10f0f).unwrap()
    );
}

#[axtest]
fn rdif_pcie_bar_allocator_prefers_matching_windows_and_falls_back_to_mem32() {
    let mut allocator = SimpleBarAllocator::default();
    ax_assert_eq!(allocator.alloc_memory32(0x1000, false), None);
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

    ax_assert_eq!(allocator.alloc_memory64(0x1000, false), Some(0x8_0000_0000));
    ax_assert_eq!(allocator.alloc_memory32(0x1000, false), Some(0x1000_0000));
    ax_assert_eq!(allocator.alloc_memory64(0x1000, true), Some(0x8_0000_1000));
    ax_assert_eq!(allocator.alloc_memory32(0x1000, true), Some(0x2000_0000));
    ax_assert_eq!(allocator.alloc_memory32(0x4000, false), None);
}
