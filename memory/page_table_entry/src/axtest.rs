use ax_memory_addr::PhysAddr;
use axtest::prelude::*;

#[cfg(target_arch = "x86_64")]
use crate::x86_64::{PTF, X64PTE};
use crate::{GenericPTE, MappingFlags};

#[axtest]
fn page_table_entry_mapping_flags_debug_and_bit_rules_hold() {
    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    ax_assert!(flags.contains(MappingFlags::READ));
    ax_assert!(flags.contains(MappingFlags::WRITE));
    ax_assert!(flags.contains(MappingFlags::USER));
    ax_assert!(!flags.contains(MappingFlags::EXECUTE));
    let debug = alloc::format!("{flags:?}");
    ax_assert!(debug.contains("READ"));
    ax_assert!(debug.contains("WRITE"));
    ax_assert!(debug.contains("USER"));
    ax_assert_eq!(flags.bits(), 0b1011);
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn page_table_entry_x64_flags_roundtrip_and_empty_rules_hold() {
    ax_assert_eq!(MappingFlags::from(PTF::empty()), MappingFlags::empty());

    let flags = MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER;
    let pt_flags = PTF::from(flags);
    ax_assert!(pt_flags.contains(PTF::PRESENT));
    ax_assert!(pt_flags.contains(PTF::WRITABLE));
    ax_assert!(pt_flags.contains(PTF::USER_ACCESSIBLE));
    ax_assert!(pt_flags.contains(PTF::NO_EXECUTE));
    ax_assert_eq!(
        MappingFlags::from(pt_flags),
        MappingFlags::READ | MappingFlags::WRITE | MappingFlags::USER
    );

    let executable = PTF::from(MappingFlags::READ | MappingFlags::EXECUTE);
    ax_assert!(!executable.contains(PTF::NO_EXECUTE));
    let device = PTF::from(MappingFlags::READ | MappingFlags::DEVICE);
    ax_assert!(device.contains(PTF::NO_CACHE));
    ax_assert!(device.contains(PTF::WRITE_THROUGH));
}

#[cfg(target_arch = "x86_64")]
#[axtest]
fn page_table_entry_x64_pte_lifecycle_rules_hold() {
    let mut entry = X64PTE::empty();
    ax_assert!(entry.is_unused());
    ax_assert!(!entry.is_present());
    ax_assert!(!entry.is_huge());

    entry = X64PTE::new_page(
        PhysAddr::from(0x1234_5000),
        MappingFlags::READ | MappingFlags::WRITE,
        true,
    );
    ax_assert!(entry.is_present());
    ax_assert!(entry.is_huge());
    ax_assert_eq!(entry.paddr(), PhysAddr::from(0x1234_5000));
    ax_assert!(entry.flags().contains(MappingFlags::READ));
    ax_assert!(entry.flags().contains(MappingFlags::WRITE));

    entry.set_paddr(PhysAddr::from(0x2000_0000));
    entry.set_flags(
        MappingFlags::READ | MappingFlags::EXECUTE | MappingFlags::USER,
        false,
    );
    ax_assert_eq!(entry.paddr(), PhysAddr::from(0x2000_0000));
    ax_assert!(!entry.is_huge());
    ax_assert!(entry.flags().contains(MappingFlags::EXECUTE));
    ax_assert!(entry.flags().contains(MappingFlags::USER));
    ax_assert!(entry.bits() != 0);
    ax_assert!(alloc::format!("{entry:?}").contains("X64PTE"));

    let table = X64PTE::new_table(PhysAddr::from(0x3000_0000));
    ax_assert!(table.is_present());
    ax_assert!(!table.is_huge());
    ax_assert_eq!(table.paddr(), PhysAddr::from(0x3000_0000));

    entry.clear();
    ax_assert!(entry.is_unused());
}
