use super::*;

#[test]
fn std_build_platform_feature_stays_on_arceos_rust_dependency() {
    let mut info = BuildInfo {
        features: vec!["ax-std/plat-dyn".to_string(), "alloc".to_string()],
        ..BuildInfo::default()
    };

    info.resolve_std_features();
    let mut envs = HashMap::new();
    pass_std_build_nested_features(&mut envs, &mut info.features, &[], &["alloc".to_string()]);

    assert_eq!(info.features, vec!["ax-std/alloc".to_string()]);
    assert!(envs.is_empty());
}

#[test]
fn x86_64_defaults_to_dynamic_platform() {
    assert!(supports_platform_dynamic("x86_64-unknown-none"));
    assert!(!default_to_bin_for_target("x86_64-unknown-none"));
}

#[test]
fn loongarch64_defaults_to_dynamic_platform_when_supported() {
    assert!(supports_platform_dynamic(
        "loongarch64-unknown-none-softfloat"
    ));
    assert!(!default_to_bin_for_target(
        "loongarch64-unknown-none-softfloat"
    ));
}

#[test]
fn unsupported_targets_do_not_effectively_enable_dynamic_platform() {
    assert!(!supports_platform_dynamic("armv7-unknown-none-eabi"));
}

#[test]
fn build_cargo_args_uses_builtin_target_and_build_std() {
    let args = BuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", &[]);

    assert!(
        args.windows(2)
            .any(|pair| pair == ["-Z", "build-std=core,alloc"])
    );
    assert!(!args.iter().any(|arg| arg.contains("-Tlinker.x")));
    assert!(!args.iter().any(|arg| arg.contains("-Taxplat.x")));
    assert!(!args.iter().any(|arg| arg.contains("-Truntime.x")));
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
