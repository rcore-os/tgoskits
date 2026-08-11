use std::{fs, path::Path};

#[test]
fn ax_sync_never_selects_an_in_crate_host_lock_engine() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !crate_root.join("src/host.rs").exists(),
        "ax-sync must not retain a second host lock algorithm"
    );

    for path in ["src/lib.rs", "src/interface/mod.rs"] {
        let source = fs::read_to_string(crate_root.join(path)).expect("read ax-sync source");
        assert!(
            !source.contains("crate::host") && !source.contains("mod host"),
            "{path} must always dispatch through the external provider"
        );
    }
}
