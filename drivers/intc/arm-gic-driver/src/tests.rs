extern crate std;

use core::cell::Cell;

#[cfg(target_arch = "aarch64")]
use crate::version::v3::test_layout::{LPI, RedistributorV3, RedistributorV4, SGI};
use crate::{
    CheckedIntIdError, IntId, checked_intid,
    define::{NmiAttributeSlot, Trigger, enable_nmi_attribute_bit, nmi_attribute_slot},
    fdt_parse_irq_config,
};

#[cfg(target_arch = "aarch64")]
#[test]
fn size_lpi() {
    let size = size_of::<LPI>();
    assert_eq!(size, 0x10000);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn size_sgi() {
    assert_eq!(size_of::<SGI>(), 0x10000);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn test_v3_rd() {
    let size = size_of::<RedistributorV3>();
    assert_eq!(size, 0x20000);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn test_v4_rd() {
    assert_eq!(size_of::<RedistributorV4>(), 0x40000);
}

#[test]
#[should_panic]
fn test_sgi() {
    let id = IntId::sgi(40);
    assert_eq!(id.is_sgi(), true);
}

#[test]
#[should_panic]
fn test_ppi() {
    let id = IntId::ppi(17);
    assert_eq!(id.is_private(), true);
}

#[test]
fn checked_intid_rejects_special_and_out_of_range_intids() {
    assert_eq!(checked_intid(1019, 1020).unwrap().to_u32(), 1019);
    assert_eq!(checked_intid(1020, 1024), Err(CheckedIntIdError));
    assert_eq!(checked_intid(4096, 1024), Err(CheckedIntIdError));
}

#[test]
fn fdt_spi_level_high_uses_gic_intid_numbering() {
    const GIC_SPI: u32 = 0;
    const RK3588_SDMMC_SPI: u32 = 203;
    const IRQ_TYPE_LEVEL_HIGH: u32 = 4;

    let config = fdt_parse_irq_config(&[GIC_SPI, RK3588_SDMMC_SPI, IRQ_TYPE_LEVEL_HIGH]).unwrap();

    assert_eq!(config.id.to_u32(), 235);
    assert_eq!(config.trigger, Trigger::Level);
}

#[test]
fn nmi_attribute_slots_cover_private_and_shared_interrupts() {
    assert_eq!(
        nmi_attribute_slot(IntId::sgi(5)),
        Some(NmiAttributeSlot::Redistributor { mask: 1 << 5 })
    );
    assert_eq!(
        nmi_attribute_slot(IntId::ppi(14)),
        Some(NmiAttributeSlot::Redistributor { mask: 1 << 30 })
    );
    assert_eq!(
        nmi_attribute_slot(IntId::spi(42)),
        Some(NmiAttributeSlot::Distributor {
            register: 2,
            mask: 1 << 10,
        })
    );
}

#[test]
fn nmi_attribute_slot_rejects_special_and_out_of_range_intids() {
    assert_eq!(nmi_attribute_slot(unsafe { IntId::raw(1023) }), None);
    assert_eq!(nmi_attribute_slot(unsafe { IntId::raw(4096) }), None);
}

#[test]
fn nmi_enable_preserves_sibling_attribute_bits() {
    let original = (1 << 2) | (1 << 19);
    let mask = 1 << 14;
    let register = Cell::new(original);

    assert!(enable_nmi_attribute_bit(
        || register.get(),
        |value| register.set(value),
        mask,
    ));
    assert_eq!(register.get(), original | mask);
}

#[test]
fn nmi_enable_rejects_raz_wi_attribute_bit() {
    let mask = 1 << 14;
    let attempted_write = Cell::new(None);

    assert!(!enable_nmi_attribute_bit(
        || 0,
        |value| attempted_write.set(Some(value)),
        mask,
    ));
    assert_eq!(attempted_write.get(), Some(mask));
}
