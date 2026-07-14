use std::{fs, path::Path};

#[test]
fn vplic_is_a_pure_software_device_model() {
    let manifest = include_str!("../Cargo.toml");

    assert!(
        !manifest.contains("riscv-h"),
        "the vPLIC device model must not depend on the live hypervisor CSR crate"
    );
    assert_sources_exclude(Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
}

fn assert_sources_exclude(path: impl AsRef<Path>) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_sources_exclude(path);
            continue;
        }
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        for forbidden in [
            "riscv_h::register",
            "hvip::",
            "vscause::",
            "sync_all_guest_contexts_vseip",
        ] {
            assert!(
                !source.contains(forbidden),
                "{} contains live CPU register operation `{forbidden}`",
                path.display()
            );
        }
    }
}
