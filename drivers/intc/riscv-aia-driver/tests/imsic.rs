//! Public geometry contract; this does not execute privileged IMSIC CSRs.
use riscv_aia_driver::{Error, FileLayout};

#[test]
fn resolve_sparse_supervisor_files_across_guest_and_group_spacing() {
    let layout = FileLayout::new(2, 7, 1, 24).unwrap();
    let base = 0x2800_0000;
    let stride = 1 << 19;
    let ranges = [
        (base, Some(0x1000)),
        (base + 2 * stride, Some(0x1000)),
        (base + (1 << 24) + stride, Some(0x1000)),
    ];
    let mut addresses = [0; 3];
    layout
        .file_addresses(ranges.into_iter(), &mut addresses)
        .unwrap();
    let targets = addresses.map(|address| layout.target(address).unwrap());
    assert_eq!(targets, [0, 2, 5]);

    // A guest page must never become a supervisor target, and duplicate file
    // ranges cannot supply independent CPU identities. Failed output is unused.
    assert_eq!(layout.target(base + 0x1000), Err(Error::InvalidLayout));
    assert_eq!(
        layout.file_addresses([ranges[0], ranges[0]].into_iter(), &mut [0; 2]),
        Err(Error::InvalidFileRange),
    );
    assert_eq!(
        layout.file_addresses([ranges[0]].into_iter(), &mut [0; 2]),
        Err(Error::InsufficientFiles),
    );
}
