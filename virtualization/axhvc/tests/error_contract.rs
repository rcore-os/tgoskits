use std::{fs, path::Path};

use axhvc::{HyperCallCode, HyperCallError, HyperCallResult, InvalidHyperCallCode};

#[test]
fn crate_uses_typed_errors_without_errno_contracts() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    let sources = fs::read_to_string(crate_dir.join("src/lib.rs")).unwrap()
        + &fs::read_to_string(crate_dir.join("src/error.rs")).unwrap();

    let errno_package = ["ax", "-errno"].concat();
    let errno_module = ["ax", "_errno"].concat();
    assert!(!manifest.contains(&errno_package));
    assert!(!sources.contains(&errno_module));
    assert!(manifest.contains("thiserror = { workspace = true }"));

    fn assert_error_contract<T: core::error::Error + Clone + Eq>() {}
    fn assert_result_alias(_: HyperCallResult<usize>) {}
    assert_error_contract::<InvalidHyperCallCode>();
    assert_error_contract::<HyperCallError>();
    assert_result_alias(Ok(0));
}

#[test]
fn errors_preserve_code_resource_and_guest_address_context() {
    let invalid: HyperCallError = InvalidHyperCallCode(0xff).into();
    assert!(matches!(invalid, HyperCallError::InvalidCode(_)));
    assert!(invalid.to_string().contains("0xff"));

    let conflict = HyperCallError::ResourceConflict {
        code: HyperCallCode::HIVCPublishChannel,
        resource: "IVC channel 7".into(),
        detail: "already published".into(),
    };
    assert!(conflict.to_string().contains("IVC channel 7"));

    let access = HyperCallError::GuestMemoryAccess {
        code: HyperCallCode::HIVCSubscribChannel,
        operation: "write subscription result",
        address: 0x4000,
        detail: "unmapped guest page".into(),
    };
    assert!(access.to_string().contains("0x4000"));
}

#[test]
fn execution_error_categories_remain_matchable() {
    let code = HyperCallCode::HIVCPublishChannel;
    let errors = [
        HyperCallError::Unsupported {
            code,
            detail: "disabled by host policy".into(),
        },
        HyperCallError::InvalidParameter {
            code,
            parameter: "shm_size_ptr",
            detail: "address is not aligned".into(),
        },
        HyperCallError::InvalidState {
            code,
            detail: "channel was unpublished".into(),
        },
        HyperCallError::ResourceNotFound {
            code,
            resource: "IVC channel 7".into(),
            detail: "publisher does not own the key".into(),
        },
        HyperCallError::OutOfMemory {
            code,
            operation: "allocate shared frame",
        },
        HyperCallError::Internal {
            code,
            operation: "map shared frame",
            detail: "stage-2 mapping failed".into(),
        },
    ];

    assert!(matches!(errors[0], HyperCallError::Unsupported { .. }));
    assert!(matches!(errors[1], HyperCallError::InvalidParameter { .. }));
    assert!(matches!(errors[2], HyperCallError::InvalidState { .. }));
    assert!(matches!(errors[3], HyperCallError::ResourceNotFound { .. }));
    assert!(matches!(errors[4], HyperCallError::OutOfMemory { .. }));
    assert!(matches!(errors[5], HyperCallError::Internal { .. }));
    for error in errors {
        assert!(error.to_string().contains("HIVCPublishChannel"));
    }
}
