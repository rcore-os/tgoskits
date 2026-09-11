use super::*;

#[test]
fn std_build_rejects_removed_platform_feature() {
    let info = BuildInfo {
        features: vec!["ax-std/plat-dyn".to_string(), "alloc".to_string()],
        ..BuildInfo::default()
    };

    assert!(info.validate_features().is_err());
}

#[test]
fn build_cargo_args_uses_target_stem_as_rustflags_key() {
    let args = BuildInfo::build_cargo_args(
        "aarch64-unknown-none-softfloat",
        &["-Cforce-frame-pointers=yes".to_string()],
    );

    assert!(args.windows(2).any(|pair| {
        pair[0] == "--config"
            && pair[1].starts_with("target.aarch64-unknown-none-softfloat.rustflags=")
            && pair[1].contains("\"-Cforce-frame-pointers=yes\"")
    }));
    assert!(
        !args
            .iter()
            .any(|arg| arg.starts_with("target.") && arg.contains('/')),
        "config key must not use a removed spec path"
    );
}
