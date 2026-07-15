use std::{fs, path::Path};

use axdevice_base::{
    AccessWidth, ControllerInputId, DeviceError, InterruptControllerId, InterruptEndpoint, IrqError,
};

#[test]
fn crate_uses_typed_errors_without_errno_contracts() {
    assert_manifest_and_sources_exclude_errno(Path::new(env!("CARGO_MANIFEST_DIR")));
}

#[test]
fn device_and_irq_errors_preserve_access_context() {
    let device = DeviceError::InvalidWidth {
        expected: AccessWidth::Dword,
        actual: AccessWidth::Qword,
    };
    let irq = IrqError::InvalidInput {
        endpoint: InterruptEndpoint::Wired {
            controller: InterruptControllerId::new(2),
            input: ControllerInputId::new(9),
        },
        operation: "route",
        detail: "line is not assigned".into(),
    };

    assert!(device.to_string().contains("Dword"));
    assert!(irq.to_string().contains("line is not assigned"));
}

fn assert_manifest_and_sources_exclude_errno(crate_dir: &Path) {
    let forbidden = ["ax", "errno"].join("_");
    let manifest_dependency = ["ax", "errno"].join("-");
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    assert!(!manifest.contains(&manifest_dependency));
    assert_directory_excludes(&crate_dir.join("src"), &forbidden);
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
