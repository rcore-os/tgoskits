use std::{fs, path::Path};

use arm_vgic::{IntId, RegisterRegion, VgicError};
use axvm_types::AccessWidth;

#[test]
fn crate_uses_typed_errors_without_kernel_errno_contracts() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    assert!(!manifest.contains(&["ax", "errno"].join("-")));
    assert_directory_excludes(&crate_dir.join("src"), &["ax", "errno"].join("_"));
}

#[test]
fn invalid_access_error_preserves_register_context() {
    let error = VgicError::InvalidAccess {
        region: RegisterRegion::Distributor,
        operation: "read",
        offset: 0x80,
        width: AccessWidth::Qword,
        detail: "register requires Dword".into(),
    };

    assert!(error.to_string().contains("Distributor"));
    assert!(error.to_string().contains("0x80"));
    assert!(error.to_string().contains("Qword"));
}

#[test]
fn intid_classification_rejects_architectural_reserved_ranges() {
    assert!(matches!(IntId::new(31), Ok(IntId::Ppi(_))));
    assert!(matches!(IntId::new(32), Ok(IntId::Spi(_))));
    assert!(matches!(IntId::new(1019), Ok(IntId::Spi(_))));
    assert!(matches!(IntId::new(8192), Ok(IntId::Lpi(_))));
    for reserved in [1020, 1023, 1024, 8191] {
        assert_eq!(
            IntId::new(reserved),
            Err(VgicError::InvalidIntId { raw: reserved })
        );
    }
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
