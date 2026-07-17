use std::{fs, path::Path};

use arm_vgic::VgicError;
use axdevice_base::{AccessWidth, DeviceError};

#[test]
fn crate_uses_typed_errors_without_errno_contracts() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    assert!(!manifest.contains(&["ax", "errno"].join("-")));
    assert_directory_excludes(&crate_dir.join("src"), &["ax", "errno"].join("_"));
}

#[test]
fn invalid_vgic_access_converts_to_device_input_error() {
    let error = VgicError::InvalidAccess {
        operation: "read",
        offset: 0x80,
        width: AccessWidth::Qword,
    };
    let device: DeviceError = error.into();

    assert!(matches!(device, DeviceError::InvalidInput { .. }));
    assert!(device.to_string().contains("0x80"));
}

#[test]
fn ownership_transition_errors_preserve_device_domain_semantics() {
    let not_spi: DeviceError = VgicError::NotSpi { irq: 17 }.into();
    assert!(matches!(not_spi, DeviceError::InvalidInput { .. }));

    let busy: DeviceError = VgicError::Busy {
        operation: "begin SPI revocation",
    }
    .into();
    assert!(matches!(busy, DeviceError::ResourceBusy { .. }));
}

#[cfg(feature = "vgicv3")]
#[test]
fn spi_ownership_is_private_and_released_only_by_typed_revocation() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = fs::read_to_string(crate_dir.join("src/v3/vgicd.rs")).unwrap();

    assert!(!source.contains("pub assigned_irqs"));
    assert!(source.contains("pub fn begin_assigned_spi_revocation"));
    assert!(source.contains("pub enum SpiRevocationPoll"));
    assert!(source.contains("finish_revocation(self.batch)"));
}

#[cfg(feature = "vgicv3")]
#[test]
fn vgic_rejects_out_of_range_irq_without_panicking() {
    assert!(matches!(
        arm_vgic::v3::vgicd::VGicD::validate_irq(1024),
        Err(VgicError::InvalidIrq {
            irq: 1024,
            max: 1024,
        })
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
