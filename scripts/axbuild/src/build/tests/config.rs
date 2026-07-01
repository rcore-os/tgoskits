use super::*;

#[test]
fn load_build_info_rejects_removed_std_field() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("build.toml");
    fs::write(
        &path,
        r#"
std = true
features = []
log = "Info"

"#,
    )
    .unwrap();

    let err = load_build_info::<BuildInfo>(&path).unwrap_err();

    assert!(
        err.to_string().contains("uses removed `std` field"),
        "{err:#}"
    );
}

#[test]
fn build_info_omits_true_plat_dyn_and_serializes_false() {
    let default = toml::to_string_pretty(&BuildInfo::default()).unwrap();

    assert!(!default.contains("plat_dyn"));

    let non_dynamic = BuildInfo {
        plat_dyn: false,
        ..BuildInfo::default()
    };
    let serialized = toml::to_string_pretty(&non_dynamic).unwrap();

    assert!(serialized.contains("plat_dyn = false"));
}

pub(super) fn declares_non_dynamic_platform(content: &str) -> bool {
    toml::from_str::<toml::Table>(content)
        .ok()
        .and_then(|table| table.get("plat_dyn").and_then(|value| value.as_bool()))
        == Some(false)
}

pub(super) fn declares_static_platform(content: &str) -> bool {
    let Ok(table) = toml::from_str::<toml::Table>(content) else {
        return false;
    };
    let Some(features) = table.get("features").and_then(|value| value.as_array()) else {
        return false;
    };

    features
        .iter()
        .filter_map(|feature| feature.as_str())
        .any(is_static_platform_feature)
}

fn is_static_platform_feature(feature: &str) -> bool {
    ax_hal_platform_feature_name(feature, None).is_some_and(|platform| platform != "plat-dyn")
}

#[test]
fn declares_static_platform_ignores_removed_ax_driver_static_feature() {
    let feature = concat!("ax-driver/", "plat", "-static");
    let content = format!("features = [\"{feature}\"]\n");

    assert!(!declares_static_platform(&content));
}

pub(super) fn checked_in_build_config_roots(workspace: &Path) -> [PathBuf; 4] {
    [
        workspace.join("apps"),
        workspace.join("os/StarryOS/configs/board"),
        workspace.join("os/axvisor/configs/board"),
        workspace.join("test-suit"),
    ]
}

pub(super) fn declares_default_dynamic_platform(content: &str) -> bool {
    toml::from_str::<toml::Table>(content)
        .ok()
        .and_then(|table| table.get("plat_dyn").and_then(|value| value.as_bool()))
        == Some(true)
}

pub(super) fn checked_in_toml_files(
    roots: impl IntoIterator<Item = PathBuf>,
) -> impl Iterator<Item = PathBuf> {
    roots.into_iter().flat_map(|root| {
        WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("toml"))
            .map(|entry| entry.into_path())
    })
}
