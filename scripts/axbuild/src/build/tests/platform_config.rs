use super::*;
use crate::build::platform::resolve_platform_config_path;

#[test]
fn resolve_platform_package_prefers_custom_aarch64_myplat_dependency() {
    let workspace = temp_workspace(
        "custom-app",
        "ax-plat-aarch64-custom = { path = \"../platforms\" }\n",
    )
    .unwrap();
    add_platform_package(
        &workspace,
        "ax-plat-aarch64-custom",
        "ax-plat-aarch64-custom",
    )
    .unwrap();

    let metadata = metadata_for_manifest_with_deps(&workspace.join("Cargo.toml"));
    let platform = resolve_platform_package(
        "custom-app",
        "aarch64-unknown-none-softfloat",
        &["myplat".to_string()],
        &metadata,
    )
    .unwrap();

    assert_eq!(platform, "ax-plat-aarch64-custom");
}

#[test]
fn resolve_platform_config_path_uses_dependency_config() {
    let workspace = temp_workspace(
        "custom-app",
        "ax-plat-aarch64-custom = { path = \"../platforms\" }\n",
    )
    .unwrap();
    add_platform_package(
        &workspace,
        "ax-plat-aarch64-custom",
        "ax-plat-aarch64-custom",
    )
    .unwrap();

    let manifest_path = workspace.join("Cargo.toml");
    let metadata = metadata_for_manifest(&manifest_path);
    let deps_metadata = metadata_for_manifest_with_deps(&manifest_path);
    let path =
        resolve_platform_config_path("ax-plat-aarch64-custom", &metadata, &deps_metadata).unwrap();

    assert!(path.ends_with("platforms/axconfig.toml"));
}
