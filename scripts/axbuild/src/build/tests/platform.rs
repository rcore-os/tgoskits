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
fn x86_64_defaults_to_dynamic_platform() {
    assert!(supports_platform_dynamic("x86_64-unknown-none"));
}

#[test]
fn loongarch64_defaults_to_dynamic_platform_when_supported() {
    assert!(supports_platform_dynamic(
        "loongarch64-unknown-none-softfloat"
    ));
}

#[test]
fn unsupported_targets_do_not_effectively_enable_dynamic_platform() {
    assert!(!supports_platform_dynamic("armv7-unknown-none-eabi"));
}

#[test]
fn build_cargo_args_use_json_target_and_build_std_for_all_bare_architectures() {
    for target in [
        "x86_64-unknown-none",
        "aarch64-unknown-none-softfloat",
        "riscv64gc-unknown-none-elf",
        "loongarch64-unknown-none-softfloat",
    ] {
        let resolved = bare_build_target_for(target).unwrap();
        let args = BuildInfo::build_cargo_args(target, &[]);

        assert_eq!(
            resolved.target,
            format!("scripts/targets/bare/{target}.json")
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-Z", "json-target-spec"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-Z", "build-std=core,alloc"])
        );
        assert!(!args.iter().any(|arg| arg.contains("-Tlinker.x")));
        assert!(!args.iter().any(|arg| arg.contains("-Taxplat.x")));
        assert!(!args.iter().any(|arg| arg.contains("-Truntime.x")));
    }
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

#[test]
fn build_cargo_args_does_not_pass_unstable_loongarch64_target_feature() {
    let args = BuildInfo::build_cargo_args("loongarch64-unknown-none-softfloat", &[]);

    assert!(!args.join("\n").contains("target-feature=-ual"));
}
