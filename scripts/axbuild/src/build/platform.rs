use super::{info::StdFeaturePrefixFamily, *};

#[cfg(test)]
pub(super) fn supports_platform_dynamic(target: &str) -> bool {
    target.starts_with("aarch64-")
        || target.starts_with("loongarch64-")
        || target.starts_with("riscv64")
        || target.starts_with("x86_64-")
}

pub(super) fn default_to_bin_for_target(target: &str) -> bool {
    !target.starts_with("x86_64-") && !target.starts_with("loongarch64-")
}

pub(super) fn normalize_legacy_feature_alias(feature: &str) -> String {
    if feature == "axstd" {
        "ax-std".to_string()
    } else if let Some(rest) = feature.strip_prefix("axstd/") {
        format!("ax-std/{rest}")
    } else {
        feature.to_string()
    }
}

pub(super) fn normalize_std_feature(feature: &str) -> String {
    let normalized = normalize_legacy_feature_alias(feature);
    match normalized.as_str() {
        "ax-std" => normalized,
        feature if feature.starts_with("ax-std/") => feature
            .split_once('/')
            .map(|(_, feature)| feature.to_string())
            .unwrap_or_else(|| normalized.clone()),
        feature
            if feature.starts_with("ax-hal/")
                || feature.starts_with("ax-driver/")
                || feature.starts_with("ax-runtime/") =>
        {
            normalized
        }
        feature => feature.to_string(),
    }
}

pub(super) fn is_removed_dynamic_platform_feature(feature: &str) -> bool {
    matches!(
        feature,
        "plat-dyn" | "ax-std/plat-dyn" | "ax-driver/plat-dyn"
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
            | "irq"
            | "ipi"
            | "alloc"
            | "paging"
            | "dma"
            | "tls"
            | "multitask"
            | "lockdep"
            | "task-ext"
            | "tracepoint-hooks"
            | "rr"
            | "cfs"
            | "stack-guard-page"
            | "stack-protector"
            | "fs"
            | "ext4"
            | "fat"
            | "net"
            | "vsock"
            | "aic8800"
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
    _package: &str,
    makefile_features: &[String],
) {
    if makefile_features.is_empty() {
        return;
    }
    apply_std_makefile_features(build_info, makefile_features);
}

pub(crate) fn apply_makefile_features_with_metadata(
    build_info: &mut BuildInfo,
    _package: &str,
    makefile_features: &[String],
    _metadata: &Metadata,
) {
    apply_std_makefile_features(build_info, makefile_features);
}

#[cfg(test)]
pub(super) fn apply_makefile_features_with_prefix_family(
    build_info: &mut BuildInfo,
    _package: &str,
    makefile_features: &[String],
    _prefix_family: anyhow::Result<StdFeaturePrefixFamily>,
) {
    if makefile_features.is_empty() {
        return;
    }

    apply_std_makefile_features(build_info, makefile_features);
}

pub(super) fn apply_std_makefile_features(
    build_info: &mut BuildInfo,
    makefile_features: &[String],
) {
    for feature in makefile_features {
        let mapped = normalize_std_feature(feature);
        if is_removed_dynamic_platform_feature(&mapped) {
            continue;
        }
        if !build_info
            .features
            .iter()
            .any(|existing| existing == &mapped)
        {
            build_info.features.push(mapped);
        }
    }
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

pub(super) fn feature_family_from_existing_features(
    features: &[String],
) -> Option<StdFeaturePrefixFamily> {
    if features
        .iter()
        .any(|feature| feature.starts_with("ax-std/"))
    {
        return Some(StdFeaturePrefixFamily::AxStd);
    }
    None
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

pub(super) fn metadata_package<'a>(metadata: &'a Metadata, package: &str) -> Option<&'a Package> {
    metadata.packages.iter().find(|pkg| pkg.name == package)
}

pub(super) fn detect_std_feature_prefix_family(
    package: &str,
    metadata: &Metadata,
) -> anyhow::Result<StdFeaturePrefixFamily> {
    let package_info = workspace_package(metadata, package)?;

    let has_axstd = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "ax-std" || dep.rename.as_deref() == Some("ax-std"));

    if has_axstd {
        Ok(StdFeaturePrefixFamily::AxStd)
    } else {
        Err(anyhow::anyhow!(
            "package `{package}` must directly depend on `ax-std`"
        ))
    }
}

#[cfg(test)]
pub(super) fn ax_hal_platform_feature_name<'a>(
    feature: &'a str,
    metadata: Option<&Metadata>,
) -> Option<&'a str> {
    let platform = feature.strip_prefix("ax-hal/")?;
    match platform {
        _ if metadata
            .map(|metadata| platform_package_by_name(metadata, platform).is_some())
            .unwrap_or_else(|| is_known_ax_hal_platform_feature(platform)) =>
        {
            Some(platform)
        }
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn is_known_ax_hal_platform_feature(_platform: &str) -> bool {
    false
}

#[cfg(test)]
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
pub(super) struct AxplatMetadata {
    platform: String,
    arch: String,
    default_for_arch: bool,
    dynamic: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
pub(super) struct AxstdMetadata {
    features: Vec<String>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub(super) struct PlatformPackage {
    package: String,
    metadata: AxplatMetadata,
}

#[cfg(test)]
pub(super) fn platform_metadata(package: &Package) -> Option<AxplatMetadata> {
    package
        .metadata
        .get("axplat")
        .cloned()
        .and_then(|metadata| serde_json::from_value(metadata).ok())
}

pub(super) fn axstd_metadata(package: &Package) -> Option<AxstdMetadata> {
    package
        .metadata
        .get("axstd")
        .cloned()
        .and_then(|metadata| serde_json::from_value(metadata).ok())
}

pub(super) fn std_package_metadata_features(package: &str, metadata: &Metadata) -> Vec<String> {
    metadata_package(metadata, package)
        .and_then(axstd_metadata)
        .map(|metadata| metadata.features)
        .unwrap_or_default()
}

#[cfg(test)]
pub(super) fn platform_packages(metadata: &Metadata) -> Vec<PlatformPackage> {
    metadata
        .packages
        .iter()
        .filter_map(|package| {
            let metadata = platform_metadata(package)?;
            Some(PlatformPackage {
                package: package.name.to_string(),
                metadata,
            })
        })
        .collect()
}

#[cfg(test)]
pub(super) fn platform_package_by_name(metadata: &Metadata, platform_name: &str) -> Option<String> {
    platform_packages(metadata)
        .into_iter()
        .find(|platform| platform.metadata.platform == platform_name)
        .map(|platform| platform.package)
}
