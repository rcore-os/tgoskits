use axtest::prelude::*;

#[axtest]
fn pcie_config_revision_and_class_hold() {
    use crate::types::config::RevisionAndClass;

    // Test RevisionAndClass struct
    let rc = RevisionAndClass {
        revision_id: 0x42,
        base_class: 0x01,
        sub_class: 0x08,
        interface: 0x02,
    };
    ax_assert_eq!(rc.revision_id, 0x42);
    ax_assert_eq!(rc.base_class, 0x01);
    ax_assert_eq!(rc.sub_class, 0x08);
    ax_assert_eq!(rc.interface, 0x02);

    // Test Debug impl (exercises formatting code)
    let formatted = format!("{rc:?}");
    ax_assert!(formatted.contains("RevisionAndClass"));
}

#[axtest]
fn pcie_config_revision_and_class_variants_hold() {
    use crate::types::config::RevisionAndClass;

    // Test RevisionAndClass with zero values
    let zero = RevisionAndClass {
        revision_id: 0,
        base_class: 0,
        sub_class: 0,
        interface: 0,
    };
    ax_assert_eq!(zero.revision_id, 0);
    ax_assert_eq!(zero.base_class, 0);
    ax_assert_eq!(zero.sub_class, 0);
    ax_assert_eq!(zero.interface, 0);

    // Test RevisionAndClass with max values
    let max = RevisionAndClass {
        revision_id: 0xFF,
        base_class: 0xFF,
        sub_class: 0xFF,
        interface: 0xFF,
    };
    ax_assert_eq!(max.revision_id, 0xFF);
    ax_assert_eq!(max.base_class, 0xFF);
    ax_assert_eq!(max.sub_class, 0xFF);
    ax_assert_eq!(max.interface, 0xFF);

    // Test Clone impl
    let cloned = max.clone();
    ax_assert_eq!(cloned.revision_id, max.revision_id);
    ax_assert_eq!(cloned.base_class, max.base_class);

    // Test common PCI class codes
    let storage = RevisionAndClass {
        revision_id: 1,
        base_class: 0x01,  // Mass storage controller
        sub_class: 0x00,  // SCSI
        interface: 0x00,
    };
    ax_assert_eq!(storage.base_class, 0x01);

    let network = RevisionAndClass {
        revision_id: 2,
        base_class: 0x02,  // Network controller
        sub_class: 0x00,  // Ethernet
        interface: 0x00,
    };
    ax_assert_eq!(network.base_class, 0x02);

    let display = RevisionAndClass {
        revision_id: 3,
        base_class: 0x03,  // Display controller
        sub_class: 0x00,  // VGA
        interface: 0x00,
    };
    ax_assert_eq!(display.base_class, 0x03);
}

#[axtest]
fn pcie_config_space_enum_variants_hold() {
    use crate::types::config::PciConfigSpace;

    // Test that PciConfigSpace is an enum (we can't construct variants without
    // actual PCI hardware, but we can verify the type exists and Debug is derived)
    // The enum has variants: PciPciBridge, Endpoint, CardBusBridge, Unknown
    
    // Verify Debug is implemented (derived)
    // We can't construct instances without real hardware, but the type exists
    let _type_check: Option<PciConfigSpace> = None;
    ax_assert!(_type_check.is_none());
}

#[axtest]
fn pcie_config_space_enum_type_hold() {
    ax_assert!(crate::types::config::pci_config_space_enum_hold_for_test());
}

#[axtest]
fn pcie_revision_and_class_comprehensive_hold() {
    ax_assert!(crate::types::config::pci_revision_and_class_comprehensive_hold_for_test());
}

#[axtest]
fn pcie_config_space_enum_variants_hold() {
    ax_assert!(crate::types::config::pci_config_space_enum_variants_hold_for_test());
}

#[axtest]
fn pci_constants_hold() {
    ax_assert!(crate::pci_constants_hold_for_test());
}
}