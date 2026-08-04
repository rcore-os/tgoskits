use core::ptr::NonNull;

use axtest::prelude::*;

use crate::{MapError, MmioAddr, MmioRaw};

#[axtest]
fn mmio_api_raw_mapping_metadata_read_write_and_formatting_hold() {
    let mut backing = [0_u8; 16];
    let ptr = NonNull::new(backing.as_mut_ptr()).unwrap();
    let raw = unsafe { MmioRaw::new(MmioAddr::from(0x1000usize), ptr, backing.len()) };

    ax_assert_eq!(raw.phys_addr(), MmioAddr::from(0x1000usize));
    ax_assert_eq!(raw.phys_addr().as_usize(), 0x1000);
    ax_assert_eq!(raw.size(), 16);
    ax_assert_eq!(raw.as_ptr(), ptr.as_ptr());
    ax_assert_eq!(raw.as_nonnull_ptr(), ptr);
    ax_assert_eq!(raw.as_slice(), &[0; 16]);
    ax_assert_eq!(alloc::format!("{}", raw.phys_addr()), "0x1000");
    ax_assert_eq!(alloc::format!("{:?}", raw.phys_addr()), "PhysAddr(0x1000)");

    raw.write::<u32>(4, 0x1122_3344);
    ax_assert_eq!(raw.read::<u32>(4), 0x1122_3344);
    ax_assert_eq!(&backing[4..8], &0x1122_3344_u32.to_ne_bytes());

    let displayed = alloc::format!("{raw}");
    ax_assert!(displayed.contains("Mmio [0x1000, 0x1010)"));
    ax_assert!(displayed.contains("virt:"));
}

#[axtest]
fn mmio_api_address_conversions_and_error_messages_hold() {
    let from_u64 = MmioAddr::from(0x2000_u64);
    let from_usize = MmioAddr::from(0x2000_usize);
    ax_assert_eq!(from_u64, from_usize);
    let raw: usize = from_usize.into();
    ax_assert_eq!(raw, 0x2000);
    ax_assert!(from_usize > MmioAddr::default());

    ax_assert_eq!(
        alloc::format!("{}", MapError::Invalid),
        "Invalid MMIO address or size"
    );
    ax_assert_eq!(
        alloc::format!("{}", MapError::NoMemory),
        "Failed to allocate memory for MMIO mapping"
    );
    ax_assert_eq!(
        alloc::format!("{}", MapError::Busy),
        "MMIO address is already in use"
    );
}
