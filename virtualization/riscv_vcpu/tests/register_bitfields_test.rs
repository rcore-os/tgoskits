use std::{fs, path::Path};

#[test]
fn register_helpers_are_typed_and_testable() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = read_rust_sources(&manifest_dir.join("src"));

    for expected in [
        "register_bitfields!",
        "hgatp_value",
        "guest_page_fault_addr",
        "delegated_exception_bits",
        "delegated_interrupt_bits",
    ] {
        assert!(
            source.contains(expected),
            "riscv_vcpu should define typed register helper `{expected}`"
        );
    }
}

#[test]
fn raw_hgatp_encoding_is_not_open_coded_in_vcpu() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let vcpu = fs::read_to_string(manifest_dir.join("src/vcpu.rs")).unwrap();

    assert!(
        !vcpu.contains("config.mode << 60"),
        "hgatp encoding must go through the typed helper"
    );
}

fn read_rust_sources(src_dir: &Path) -> String {
    let mut combined = String::new();
    for entry in fs::read_dir(src_dir).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            combined.push_str(&fs::read_to_string(path).unwrap());
            combined.push('\n');
        }
    }
    combined
}
