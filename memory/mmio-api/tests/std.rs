use core::ptr::NonNull;

use mmio_api::{MmioAddr, MmioRaw};

#[test]
fn mmio_api_raw_mapping_reads_and_writes_backing_storage() {
    let mut backing = [0_u8; 16];
    let ptr = NonNull::new(backing.as_mut_ptr()).unwrap();
    let raw = unsafe { MmioRaw::new(MmioAddr::from(0x1000usize), ptr, backing.len()) };

    raw.write::<u32>(4, 0x1122_3344);
    assert_eq!(raw.read::<u32>(4), 0x1122_3344);
    assert_eq!(&backing[4..8], &0x1122_3344_u32.to_ne_bytes());
}
