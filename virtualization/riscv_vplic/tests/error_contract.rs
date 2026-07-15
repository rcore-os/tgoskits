use std::{fs, path::Path};

use axdevice_base::{AccessWidth, DeviceError};
use riscv_vplic::VplicError;

#[test]
fn crate_uses_typed_errors_without_errno_contracts() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    assert!(!manifest.contains(&["ax", "errno"].join("-")));
    assert_directory_excludes(&crate_dir.join("src"), &["ax", "errno"].join("_"));
}

#[test]
fn invalid_vplic_width_converts_to_device_input_error() {
    let error = VplicError::InvalidAccessWidth {
        expected: AccessWidth::Dword,
        actual: AccessWidth::Qword,
    };
    let device: DeviceError = error.into();

    assert!(matches!(device, DeviceError::InvalidInput { .. }));
    assert!(device.to_string().contains("Qword"));
}

fn assert_directory_excludes(directory: &Path, forbidden: &str) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_directory_excludes(&path, forbidden);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let source = fs::read_to_string(&path).unwrap();
            assert!(!source.contains(forbidden), "{}", path.display());
        }
    }
}
