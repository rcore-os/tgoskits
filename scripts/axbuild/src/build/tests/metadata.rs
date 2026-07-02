use super::*;
use crate::build::info::AxFeaturePrefixFamily;

#[test]
fn detects_axfeat_direct_dependency_via_metadata() {
    let workspace = temp_workspace("ax-feat-app", "ax-feat = \"0.1.0\"\n").unwrap();

    let metadata = metadata_for_manifest(&workspace.join("Cargo.toml"));
    let family = detect_ax_feature_prefix_family("ax-feat-app", &metadata).unwrap();

    assert_eq!(family, AxFeaturePrefixFamily::AxFeat);
}

#[test]
fn std_build_maps_arceos_features_to_ax_std_dependency() {
    let mut info = BuildInfo {
        features: vec![
            "ax-std".to_string(),
            "lockdep".to_string(),
            "ax-std/smp".to_string(),
        ],
        ..BuildInfo::default()
    };

    info.resolve_std_features();
    let mut envs = HashMap::new();
    pass_std_build_nested_features(
        &mut envs,
        &mut info.features,
        &[],
        &[
            "lockdep".to_string(),
            "smp".to_string(),
            "std-compat".to_string(),
        ],
    );

    assert_eq!(
        info.features,
        vec![
            "ax-std/lockdep".to_string(),
            "ax-std/smp".to_string(),
            "ax-std/std-compat".to_string()
        ]
    );
    assert!(envs.is_empty());
    assert!(!envs.values().any(|value| value.contains("arceos")));
    assert!(!info.features.contains(&"lockdep".to_string()));
}

#[test]
fn makefile_features_use_ax_std_dependency_for_std_build() {
    let mut info = BuildInfo {
        features: Vec::new(),
        ..BuildInfo::default()
    };

    apply_makefile_features_with_prefix_family(
        &mut info,
        "arceos-app",
        &[String::from("lockdep")],
        Err(anyhow::anyhow!("std test packages do not depend on ax-std")),
    );

    info.resolve_std_features();
    let mut envs = HashMap::new();
    pass_std_build_nested_features(
        &mut envs,
        &mut info.features,
        &[],
        &["lockdep".to_string(), "std-compat".to_string()],
    );

    assert_eq!(
        info.features,
        vec![
            "ax-std/lockdep".to_string(),
            "ax-std/std-compat".to_string()
        ]
    );
    assert!(envs.is_empty());
}

#[test]
fn retired_static_platform_features_are_not_ax_hal_platforms() {
    let metadata = repo_metadata();

    for feature in [
        "ax-hal/aarch64-qemu-virt",
        "ax-hal/aarch64-raspi",
        "ax-hal/aarch64-bsta1000b",
        "ax-hal/aarch64-phytium-pi",
        "ax-hal/riscv64-visionfive2",
    ] {
        assert_eq!(ax_hal_platform_feature_name(feature, Some(&metadata)), None);
    }
}

#[test]
fn default_platform_feature_uses_dynamic_platform() {
    let mut info = BuildInfo::default();

    info.resolve_features_with_prefix_family(
        "arceos-helloworld",
        "loongarch64-unknown-none-softfloat",
        Ok(AxFeaturePrefixFamily::AxStd),
        None,
    );

    assert!(info.features.contains(&"ax-std/plat-dyn".to_string()));
}
