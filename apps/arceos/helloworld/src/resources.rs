//! A resource-only application: no networking, OTA, or handoff producer.
use someboot::boot_resources::{self, PhysicalRange, Placement, ResourceId, ResourceRequest};
const SCRATCH: ResourceId = ResourceId(0x5343_5241_5443_4801);
someboot::declare_boot_resources! {
    ResourceRequest::new(SCRATCH, 64 * 1024, 64 * 1024, Placement::Anywhere),
}
pub fn exercise() {
    let first = boot_resources::allocate(SCRATCH, 4096, 4096).expect("declared scratch arena");
    assert!(first.range().start.is_multiple_of(64 * 1024));
    let pointer = someboot::mem::phys_to_virt(first.range().start as usize);
    // SAFETY: first uniquely owns these reserved writable bytes until this function returns.
    unsafe {
        core::ptr::write_bytes(pointer, 0xa5, 4096);
    }
    let second = boot_resources::allocate(SCRATCH, 4096, 4096).unwrap();
    assert!(!first.range().overlaps(second.range()));
    let released = second.range();
    drop(second);
    let reused = boot_resources::allocate(SCRATCH, 4096, 4096).unwrap();
    assert_eq!(reused.range(), released);
    assert!(boot_resources::allocate(SCRATCH, 64 * 1024, 4096).is_err());
    // SAFETY: the first allocation stayed live while later requests succeeded or failed.
    let actual = unsafe { core::slice::from_raw_parts(pointer, 4096) };
    assert!(actual.iter().all(|byte| *byte == 0xa5));
    assert!(
        first
            .range()
            .contains(PhysicalRange::new(first.range().start, 4096))
    );
    println!("BOOT_RESOURCES_PASSED");
}
