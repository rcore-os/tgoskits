use std::{fs, path::Path};

use axdevice::DeviceManagerError;
use axdevice_base::{DeviceError, RegistryError};

#[test]
fn crate_uses_typed_errors_without_errno_contracts() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    let dependency = ["ax", "errno"].join("-");
    let source_name = ["ax", "errno"].join("_");

    assert!(!manifest.contains(&dependency));
    assert_directory_excludes(&crate_dir.join("src"), &source_name);
}

#[test]
fn from_conversions_keep_device_and_registry_variants_matchable() {
    let device: DeviceManagerError = DeviceError::NotFound.into();
    let registry: DeviceManagerError = RegistryError::BusKindNotSupported {
        kind: axdevice_base::BusKind::Port,
        arch: axdevice_base::Arch::AArch64,
    }
    .into();

    assert!(matches!(
        device,
        DeviceManagerError::Device(DeviceError::NotFound)
    ));
    assert!(matches!(
        registry,
        DeviceManagerError::Registry(RegistryError::BusKindNotSupported { .. })
    ));
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
