use std::{fs, path::Path};

use axvmconfig::{AxVmConfigError, AxVmConfigResult, VMBootProtocol, VMKernelConfig};

#[test]
fn crate_uses_typed_errors_and_workspace_dependencies() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = fs::read_to_string(crate_dir.join("Cargo.toml")).unwrap();
    let sources = fs::read_to_string(crate_dir.join("src/lib.rs")).unwrap()
        + &fs::read_to_string(crate_dir.join("src/error.rs")).unwrap();

    let errno_package = ["ax", "-errno"].concat();
    let errno_module = ["ax", "_errno"].concat();
    assert!(!manifest.contains(&errno_package));
    assert!(!sources.contains(&errno_module));
    assert!(manifest.contains("thiserror = { workspace = true }"));
    assert!(manifest.contains("toml = { workspace = true"));

    fn assert_error_contract<T: core::error::Error + Clone + Eq>() {}
    fn assert_result_alias(_: AxVmConfigResult<usize>) {}
    assert_error_contract::<AxVmConfigError>();
    assert_result_alias(Ok(1));
}

#[test]
fn public_errors_preserve_parse_and_boot_context() {
    let parse_error = axvmconfig::AxVMCrateConfig::from_toml("[base").unwrap_err();
    assert!(matches!(parse_error, AxVmConfigError::TomlParse { .. }));
    assert!(parse_error.to_string().contains("VM TOML configuration"));

    let direct_with_bios = VMKernelConfig {
        enable_bios: true,
        boot_protocol: Some(VMBootProtocol::Direct),
        ..Default::default()
    };
    let conflict = direct_with_bios.validate_boot_config().unwrap_err();
    assert_eq!(
        conflict,
        AxVmConfigError::BootProtocolConflict {
            protocol: VMBootProtocol::Direct,
            enable_bios: true,
        }
    );
    assert!(conflict.to_string().contains("enable_bios = true"));
}
