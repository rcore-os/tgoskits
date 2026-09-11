use super::*;

pub(super) fn normalize_std_feature(feature: &str) -> String {
    match feature {
        "ax-std" => feature.to_string(),
        feature if feature.starts_with("ax-std/") => feature
            .split_once('/')
            .map(|(_, feature)| feature.to_string())
            .unwrap_or_else(|| feature.to_string()),
        feature
            if feature.starts_with("ax-hal/")
                || feature.starts_with("ax-driver/")
                || feature.starts_with("ax-runtime/") =>
        {
            feature.to_string()
        }
        feature => feature.to_string(),
    }
}

pub(super) fn is_removed_dynamic_platform_feature(feature: &str) -> bool {
    matches!(
        feature,
        "dyn-plat"
            | "plat-dyn"
            | "axplat-dyn"
            | "ax-hal/plat-dyn"
            | "ax-std/plat-dyn"
            | "axvm/plat-dyn"
            | "ax-driver/plat-dyn"
    )
}

pub(super) fn is_axstd_std_check_feature(feature: &str) -> bool {
    matches!(feature, "ax-std")
        || feature.starts_with("ax-hal/")
        || feature.starts_with("ax-driver/")
        || feature.starts_with("ax-runtime/")
        || is_known_axstd_feature(feature)
}

pub(super) fn std_feature_stays_on_app(feature: &str, app_features: &[String]) -> bool {
    if feature == "arceos" {
        return true;
    }
    !is_axstd_std_check_feature(feature)
        || app_features
            .iter()
            .any(|app_feature| app_feature == feature)
}

pub(super) fn is_known_axstd_feature(feature: &str) -> bool {
    matches!(
        feature,
        "smp"
            | "fp-simd"
            | "uspace"
            | "hv"
            | "ipi"
            | "alloc"
            | "paging"
            | "tls"
            | "lockdep"
            | "task-ext"
            | "tracepoint-hooks"
            | "sched-rr"
            | "sched-cfs"
            | "stack-guard-page"
            | "stack-protector"
            | "fs"
            | "ext4fs"
            | "fatfs"
            | "net"
            | "vsock"
            | "dns"
            | "display"
            | "input"
            | "usb"
            | "rtc"
            | "backtrace"
            | "dwarf"
            | "ext-ld"
            | "wake-ipi"
            | "std-compat"
    )
}

pub(super) fn is_log_level_feature(feature: &str) -> bool {
    matches!(
        feature,
        "log-level-off"
            | "log-level-error"
            | "log-level-warn"
            | "log-level-info"
            | "log-level-debug"
            | "log-level-trace"
    )
}

pub(crate) fn parse_makefile_features(input: &str) -> Vec<String> {
    let mut features = Vec::new();
    for feature in input.split(|ch: char| ch == ',' || ch.is_whitespace()) {
        let feature = feature.trim();
        if !feature.is_empty() && !features.iter().any(|existing| existing == feature) {
            features.push(feature.to_string());
        }
    }
    features
}

pub(crate) fn makefile_features_from_env() -> Vec<String> {
    std::env::var("FEATURES")
        .ok()
        .map(|value| parse_makefile_features(&value))
        .unwrap_or_default()
}

pub(crate) fn apply_makefile_features(
    build_info: &mut BuildInfo,
    makefile_features: &[String],
) -> anyhow::Result<()> {
    for feature in makefile_features {
        build_info.validate_feature(feature)?;
        let mapped = normalize_std_feature(feature);
        if !build_info
            .features
            .iter()
            .any(|existing| existing == &mapped)
        {
            build_info.features.push(mapped);
        }
    }
    Ok(())
}

pub(crate) fn default_build_info_path_in_workspace(
    workspace_root: &Path,
    package: &str,
    target: &str,
) -> PathBuf {
    axbuild_tmp_dir(workspace_root)
        .join("config")
        .join(package)
        .join(format!("build-{target}.toml"))
}

pub(crate) fn workspace_metadata() -> anyhow::Result<Metadata> {
    let manifest_path = workspace_manifest_path()?;
    workspace_metadata_root_manifest(&manifest_path)
}

pub(crate) fn cached_workspace_metadata() -> anyhow::Result<&'static Metadata> {
    static METADATA: OnceLock<anyhow::Result<Metadata, String>> = OnceLock::new();

    cached_metadata_result(
        METADATA.get_or_init(|| workspace_metadata().map_err(|err| format!("{err:#}"))),
    )
}

pub(super) fn cached_metadata_result(
    result: &'static anyhow::Result<Metadata, String>,
) -> anyhow::Result<&'static Metadata> {
    result.as_ref().map_err(|err| anyhow::anyhow!("{err}"))
}

pub(super) fn workspace_package<'a>(
    metadata: &'a Metadata,
    package: &str,
) -> anyhow::Result<&'a Package> {
    metadata
        .packages
        .iter()
        .find(|pkg| metadata.workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))
}
