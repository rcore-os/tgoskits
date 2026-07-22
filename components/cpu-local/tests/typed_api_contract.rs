use std::{fs, path::PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources() -> String {
    let root = crate_root().join("src");
    let mut pending = vec![root];
    let mut source = String::new();
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                source.push_str(&fs::read_to_string(path).unwrap());
            }
        }
    }
    source
}

#[test]
fn cpu_local_has_no_versioned_or_trait_ffi_surface() {
    let source = rust_sources();
    for forbidden in [
        "CPU_LOCAL_ABI_VERSION",
        "CpuBindingV1",
        "CpuBindingResultV1",
        "CpuLocalStatus",
        "CpuLocalPlatformV1",
        "RegisterModeV1",
        "HostLevelV1",
        "cpu-local_0_1",
        "pub mod raw",
    ] {
        assert!(
            !source.contains(forbidden),
            "cpu-local still contains obsolete surface {forbidden}"
        );
    }

    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).unwrap();
    assert!(!manifest.contains("trait-ffi"));
    assert!(manifest.contains("thiserror.workspace = true"));
}

#[test]
fn public_errors_use_thiserror_without_manual_trait_impls() {
    let source = rust_sources();
    assert!(source.contains("thiserror::Error"));
    assert!(!source.contains("impl fmt::Display for Cpu"));
    assert!(!source.contains("impl core::error::Error for Cpu"));
    assert!(!source.contains("impl fmt::Display for ThreadSwitchError"));
    assert!(!source.contains("impl core::error::Error for ThreadSwitchError"));
}

#[test]
fn pin_is_scoped_and_cannot_be_forged_directly() {
    let pin = fs::read_to_string(crate_root().join("src/pin.rs")).unwrap();
    assert!(pin.contains("pub struct CpuPin<'scope>"));
    assert!(pin.contains("pub struct ExclusiveCpu<'pin>"));
    assert!(pin.contains("pub unsafe fn with_cpu_pin"));
    assert!(!pin.contains("CpuPin::new_unchecked"));
    assert!(!pin.contains("pub const unsafe fn new_unchecked"));
}
