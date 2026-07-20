use std::{fs, path::PathBuf};

#[test]
fn non_multitask_tls_is_owned_by_a_named_per_cpu_cell() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(root.join("src/lib.rs")).unwrap();

    assert!(source.contains("static MAIN_TLS: ax_lazyinit::LazyInit<ax_hal::tls::TlsArea>"));
    assert!(source.contains("MAIN_TLS.current_ref_raw()"));
    assert!(source.contains("main_tls.init_once(ax_hal::tls::TlsArea::alloc())"));
    assert!(!source.contains("core::mem::forget(main_tls)"));
}
