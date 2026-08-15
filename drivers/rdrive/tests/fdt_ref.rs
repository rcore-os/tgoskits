use core::ptr::NonNull;

use fdt_edit::Fdt;
use rdrive::{Platform, with_fdt};

/// `fdt_ref()` hands out a stable `'static` borrow of the parsed device tree
/// with no per-call clone, and is `None` before any FDT source is initialized.
#[test]
fn fdt_ref_borrows_without_cloning() {
    // Nothing to borrow before an FDT source is installed.
    assert!(rdrive::fdt_ref().is_none());

    // Install a minimal FDT (mirrors tests/fdt_probe.rs).
    let fdt = Fdt::new();
    let encoded = fdt.encode();
    let dtb = Box::leak(encoded.as_ref().to_vec().into_boxed_slice());
    rdrive::init(Platform::Fdt {
        addr: NonNull::new(dtb.as_mut_ptr()).unwrap(),
    })
    .expect("FDT platform should initialize");

    // After init the borrow is available, and repeated calls return the SAME
    // tree — proving it borrows the parse-once device tree rather than deep-
    // copying the ~hundreds-of-KiB blob on every call.
    let a = rdrive::fdt_ref().expect("fdt_ref after init");
    let b = rdrive::fdt_ref().expect("fdt_ref after init");
    assert!(
        core::ptr::eq(a, b),
        "fdt_ref must borrow, not clone, the device tree"
    );

    // It is the same underlying tree that `with_fdt` exposes, so switching a
    // consumer from `with_fdt(Clone::clone)` to `fdt_ref()` is semantics-
    // preserving.
    let same = with_fdt(|f| core::ptr::eq(f, a)).expect("with_fdt after init");
    assert!(same, "fdt_ref and with_fdt must borrow the same tree");
}
