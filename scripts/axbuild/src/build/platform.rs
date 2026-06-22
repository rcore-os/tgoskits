use super::{info::AxFeaturePrefixFamily, *};

pub(super) fn default_plat_dyn() -> bool {
    true
}

pub(super) fn is_default_plat_dyn(value: &bool) -> bool {
    *value
}

pub(crate) fn resolve_effective_plat_dyn(
    target: &str,
    configured_plat_dyn: bool,
    plat_dyn_override: Option<bool>,
) -> bool {
    plat_dyn_override.unwrap_or(configured_plat_dyn) && supports_platform_dynamic(target)
}

pub(super) fn supports_platform_dynamic(target: &str) -> bool {
    target.starts_with("aarch64-")
        || target.starts_with("loongarch64-")
        || target.starts_with("riscv64")
        || target.starts_with("x86_64-")
}

pub(super) fn default_to_bin_for_target(target: &str) -> bool {
    !target.starts_with("x86_64-") && !target.starts_with("loongarch64-")
}

pub(super) fn default_to_bin_for_target_config(target: &str, plat_dyn: bool) -> bool {
    default_to_bin_for_target(target)
        || (plat_dyn && (target.starts_with("x86_64-") || target.starts_with("loongarch64-")))
}

pub(super) fn normalize_legacy_feature_alias(feature: &str) -> String {
    if feature == "axstd" {
        "ax-std".to_string()
    } else if let Some(rest) = feature.strip_prefix("axstd/") {
        format!("ax-std/{rest}")
    } else if feature == "axfeat" {
        "ax-feat".to_string()
    } else if let Some(rest) = feature.strip_prefix("axfeat/") {
        format!("ax-feat/{rest}")
    } else {
        feature.to_string()
    }
}

pub(super) fn normalize_std_feature(feature: &str) -> String {
    let normalized = normalize_legacy_feature_alias(feature);
    match normalized.as_str() {
        "ax-std" | "ax-feat" => normalized,
        feature if feature.starts_with("ax-std/") || feature.starts_with("ax-feat/") => feature
            .split_once('/')
            .map(|(_, feature)| feature.to_string())
            .unwrap_or_else(|| normalized.clone()),
        feature if feature.starts_with("ax-hal/") || feature.starts_with("ax-driver/") => {
            normalized
        }
        feature => feature.to_string(),
    }
}

pub(super) fn is_axstd_std_check_feature(feature: &str) -> bool {
    matches!(feature, "ax-std" | "ax-feat")
        || feature.starts_with("ax-hal/")
        || feature.starts_with("ax-driver/")
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
            | "myplat"
            | "defplat"
            | "plat-dyn"
            | "alloc"
            | "paging"
            | "dma"
            | "tls"
            | "multitask"
            | "lockdep"
            | "task-ext"
            | "sched-fifo"
            | "sched-rr"
            | "sched-cfs"
            | "stack-guard-page"
            | "stack-protector"
            | "fs"
            | "ext4fs"
            | "fatfs"
            | "net"
            | "dns"
            | "display"
            | "input"
            | "rtc"
            | "backtrace"
            | "dwarf"
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
    _prefix_family: anyhow::Result<AxFeaturePrefixFamily>,
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

pub(super) fn generated_axconfig_path(package: &str, target: &str) -> anyhow::Result<PathBuf> {
    Ok(axbuild_tmp_dir(&crate::context::workspace_root_path()?)
        .join("axconfig")
        .join(package)
        .join(target)
        .join(".axconfig.toml"))
}

pub(super) fn feature_family_from_existing_features(
    features: &[String],
) -> Option<AxFeaturePrefixFamily> {
    if features
        .iter()
        .any(|feature| feature.starts_with("ax-std/"))
    {
        return Some(AxFeaturePrefixFamily::AxStd);
    }
    if features
        .iter()
        .any(|feature| feature.starts_with("ax-feat/"))
    {
        return Some(AxFeaturePrefixFamily::AxFeat);
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

pub(super) fn workspace_metadata_with_deps() -> anyhow::Result<Metadata> {
    let manifest_path = workspace_manifest_path()?;
    crate::context::workspace_metadata_root_manifest_with_deps(&manifest_path)
}

pub(crate) fn cached_workspace_metadata_with_deps() -> anyhow::Result<&'static Metadata> {
    static METADATA: OnceLock<anyhow::Result<Metadata, String>> = OnceLock::new();

    cached_metadata_result(
        METADATA.get_or_init(|| workspace_metadata_with_deps().map_err(|err| format!("{err:#}"))),
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

pub(super) fn detect_ax_feature_prefix_family(
    package: &str,
    metadata: &Metadata,
) -> anyhow::Result<AxFeaturePrefixFamily> {
    let package_info = workspace_package(metadata, package)?;

    let has_axstd = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "ax-std" || dep.rename.as_deref() == Some("ax-std"));
    let has_axfeat = package_info
        .dependencies
        .iter()
        .any(|dep| dep.name == "ax-feat" || dep.rename.as_deref() == Some("ax-feat"));

    match (has_axstd, has_axfeat) {
        (true, true) | (true, false) => Ok(AxFeaturePrefixFamily::AxStd),
        (false, true) => Ok(AxFeaturePrefixFamily::AxFeat),
        (false, false) => Err(anyhow::anyhow!(
            "package `{package}` must directly depend on `ax-std` or `ax-feat`"
        )),
    }
}

pub(super) fn has_myplat_feature(features: &[String]) -> bool {
    features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "myplat" | "ax-std/myplat" | "ax-feat/myplat" | "ax-hal/myplat"
        )
    })
}

pub(super) fn has_defplat_feature(features: &[String]) -> bool {
    features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "defplat" | "ax-std/defplat" | "ax-feat/defplat" | "ax-hal/defplat"
        )
    })
}

pub(super) fn ax_hal_platform_feature_name<'a>(
    feature: &'a str,
    metadata: Option<&Metadata>,
) -> Option<&'a str> {
    let platform = feature.strip_prefix("ax-hal/")?;
    match platform {
        "plat-dyn" => Some(platform),
        _ if metadata
            .map(|metadata| platform_package_by_name(metadata, platform).is_some())
            .unwrap_or_else(|| is_known_ax_hal_platform_feature(platform)) =>
        {
            Some(platform)
        }
        _ => None,
    }
}

pub(super) fn is_known_ax_hal_platform_feature(platform: &str) -> bool {
    matches!(
        platform,
        "riscv64-sg2002" | "riscv64-visionfive2" | "loongarch64-qemu-virt"
    )
}

pub(super) fn has_ax_hal_platform_feature(
    features: &[String],
    metadata: Option<&Metadata>,
) -> bool {
    features
        .iter()
        .any(|feature| ax_hal_platform_feature_name(feature, metadata).is_some())
}

pub(super) fn default_ax_hal_platform_feature(
    target: &str,
    metadata: Option<&Metadata>,
) -> anyhow::Result<String> {
    let arch = target_arch_name(target)?;
    if let Some(metadata) = metadata
        && let Some(platform) = platform_packages(metadata)
            .into_iter()
            .find(|platform| {
                platform.metadata.arch == arch
                    && platform.metadata.default_for_arch
                    && !platform.metadata.dynamic
            })
            .map(|platform| platform.metadata.platform)
    {
        return Ok(format!("ax-hal/{platform}"));
    }

    Ok(match arch {
        "x86_64" | "aarch64" | "riscv64" => {
            return Err(anyhow!(
                "no static default ax-hal platform for arch `{arch}`"
            ));
        }
        "loongarch64" => "ax-hal/loongarch64-qemu-virt",
        _ => unreachable!("unsupported arch"),
    }
    .to_string())
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
pub(super) struct AxplatMetadata {
    platform: String,
    arch: String,
    config: Option<PathBuf>,
    default_for_arch: bool,
    dynamic: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
pub(super) struct AxstdMetadata {
    features: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct PlatformPackage {
    package: String,
    manifest_dir: PathBuf,
    metadata: AxplatMetadata,
}

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

pub(super) fn platform_packages(metadata: &Metadata) -> Vec<PlatformPackage> {
    metadata
        .packages
        .iter()
        .filter_map(|package| {
            let metadata = platform_metadata(package)?;
            let manifest_dir = Path::new(package.manifest_path.as_std_path())
                .parent()?
                .to_path_buf();
            Some(PlatformPackage {
                package: package.name.to_string(),
                manifest_dir,
                metadata,
            })
        })
        .collect()
}

pub(super) fn platform_package_by_name(metadata: &Metadata, platform_name: &str) -> Option<String> {
    platform_packages(metadata)
        .into_iter()
        .find(|platform| platform.metadata.platform == platform_name)
        .map(|platform| platform.package)
}

pub(super) fn platform_package_by_name_with_workspace_fallback(
    metadata: &Metadata,
    platform_name: &str,
) -> Option<String> {
    platform_package_by_name(metadata, platform_name).or_else(|| {
        cached_workspace_metadata()
            .ok()
            .and_then(|metadata| platform_package_by_name(metadata, platform_name))
    })
}

pub(super) fn default_platform_package(metadata: &Metadata, arch: &str) -> Option<String> {
    platform_packages(metadata)
        .into_iter()
        .find(|platform| {
            platform.metadata.arch == arch
                && platform.metadata.default_for_arch
                && !platform.metadata.dynamic
        })
        .map(|platform| platform.package)
}

pub(super) fn default_platform_package_with_workspace_fallback(
    metadata: &Metadata,
    arch: &str,
) -> Option<String> {
    default_platform_package(metadata, arch).or_else(|| {
        cached_workspace_metadata()
            .ok()
            .and_then(|metadata| default_platform_package(metadata, arch))
    })
}

pub(super) fn platform_config_path_from_metadata(
    platform_package: &str,
    metadata: &Metadata,
) -> Option<PathBuf> {
    platform_packages(metadata)
        .into_iter()
        .find(|platform| platform.package == platform_package)
        .and_then(|platform| {
            platform
                .metadata
                .config
                .map(|config| platform.manifest_dir.join(config))
        })
        .filter(|path| path.exists())
}

pub(super) fn ax_hal_platform_package(platform: &str, metadata: &Metadata) -> Option<String> {
    platform_package_by_name_with_workspace_fallback(metadata, platform)
}

pub(super) fn require_default_platform_package(
    metadata: &Metadata,
    arch: &str,
) -> anyhow::Result<String> {
    default_platform_package_with_workspace_fallback(metadata, arch)
        .ok_or_else(|| anyhow!("no default platform package is registered for arch `{arch}`"))
}

pub(super) fn resolve_platform_package(
    package: &str,
    target: &str,
    features: &[String],
    metadata: &Metadata,
) -> anyhow::Result<String> {
    let arch = target_arch_name(target)?;
    let package_info = workspace_package(metadata, package)?;

    if let Some(platform) = features
        .iter()
        .find_map(|feature| ax_hal_platform_feature_name(feature, Some(metadata)))
        .and_then(|platform| ax_hal_platform_package(platform, metadata))
    {
        return Ok(platform);
    }

    let explicit_platform_features: Vec<_> = features
        .iter()
        .map(|feature| {
            feature
                .strip_prefix("ax-feat/")
                .or_else(|| feature.strip_prefix("ax-std/"))
                .unwrap_or(feature.as_str())
        })
        .filter(|feature| {
            !matches!(
                *feature,
                "ax-std" | "ax-feat" | "plat-dyn" | "defplat" | "myplat"
            )
        })
        .collect();

    if let Some(platform) =
        explicit_platform_package_from_features(package_info, &explicit_platform_features, metadata)
    {
        return Ok(platform);
    }

    if has_myplat_feature(features)
        && let Some(dep) = package_info
            .dependencies
            .iter()
            .find(|dep| myplat_dependency_matches_arch(&dep.name, arch))
    {
        return Ok(dep.name.clone());
    }

    require_default_platform_package(metadata, arch)
}

pub(super) fn target_arch_name(target: &str) -> anyhow::Result<&'static str> {
    if target.starts_with("aarch64-") {
        Ok("aarch64")
    } else if target.starts_with("x86_64-") {
        Ok("x86_64")
    } else if target.starts_with("riscv64") {
        Ok("riscv64")
    } else if target.starts_with("loongarch64-") {
        Ok("loongarch64")
    } else {
        Err(anyhow!("unsupported target triple `{target}`"))
    }
}

pub(super) fn explicit_platform_package_from_features(
    package_info: &Package,
    explicit_features: &[&str],
    metadata: &Metadata,
) -> Option<String> {
    explicit_features
        .iter()
        .find_map(|feature| platform_package_by_name_with_workspace_fallback(metadata, feature))
        .or_else(|| {
            package_info
                .dependencies
                .iter()
                .find(|dep| {
                    dependency_is_platform(&dep.name)
                        && explicit_features.iter().any(|feature| {
                            *feature == dep.name
                                || *feature == linker_platform_name(&dep.name)
                                || feature_enables_dependency(package_info, feature, &dep.name)
                        })
                })
                .map(|dep| dep.name.clone())
        })
}

pub(super) fn dependency_is_platform(dep_name: &str) -> bool {
    dep_name.starts_with("axplat-") || dep_name.starts_with("ax-plat-")
}

pub(super) fn feature_enables_dependency(
    package_info: &Package,
    feature: &str,
    dep_name: &str,
) -> bool {
    package_info.features.get(feature).is_some_and(|items| {
        items
            .iter()
            .any(|item| item == dep_name || item == &format!("dep:{dep_name}"))
    })
}

pub(super) fn myplat_dependency_matches_arch(dep_name: &str, arch: &str) -> bool {
    myplat_dependency_prefixes_for_arch(arch)
        .iter()
        .any(|prefix| dep_name.starts_with(prefix))
}

pub(super) fn myplat_dependency_prefixes_for_arch(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["axplat-x86-", "axplat-x86_64-", "ax-plat-x86-"],
        "aarch64" => &["axplat-aarch64-", "ax-plat-aarch64-"],
        "riscv64" => &["axplat-riscv64-", "ax-plat-riscv64-"],
        "loongarch64" => &["axplat-loongarch64-", "ax-plat-loongarch64-"],
        _ => &[],
    }
}

pub(super) fn linker_platform_name(platform_package: &str) -> &str {
    platform_package
        .strip_prefix("axplat-")
        .or_else(|| platform_package.strip_prefix("ax-plat-"))
        .unwrap_or(platform_package)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedPlatformConfig {
    pub(crate) package: String,
    pub(crate) config_path: PathBuf,
    pub(crate) name: String,
}

pub(crate) fn resolve_platform_config(
    package: &str,
    target: &str,
    features: &[String],
    metadata: &Metadata,
) -> anyhow::Result<ResolvedPlatformConfig> {
    let platform_package = resolve_platform_package(package, target, features, metadata)?;
    resolve_platform_config_by_package(&platform_package, metadata)
}

pub(crate) fn resolve_platform_config_by_package(
    platform_package: &str,
    metadata: &Metadata,
) -> anyhow::Result<ResolvedPlatformConfig> {
    let deps_metadata = cached_workspace_metadata_with_deps()
        .context("failed to load dependency metadata for platform config resolution")?;
    resolve_platform_config_by_package_with_metadata(platform_package, metadata, deps_metadata)
}

pub(crate) fn resolve_platform_config_by_package_with_metadata(
    platform_package: &str,
    metadata: &Metadata,
    deps_metadata: &Metadata,
) -> anyhow::Result<ResolvedPlatformConfig> {
    let config_path = resolve_platform_config_path(platform_package, metadata, deps_metadata)?;
    let name = read_platform_name(&config_path)
        .unwrap_or_else(|| linker_platform_name(platform_package).to_string());
    Ok(ResolvedPlatformConfig {
        package: platform_package.to_string(),
        config_path,
        name,
    })
}

pub(crate) fn resolve_platform_config_path(
    platform_package: &str,
    metadata: &Metadata,
    deps_metadata: &Metadata,
) -> anyhow::Result<PathBuf> {
    if let Some(local_path) = find_local_platform_config_path(platform_package, metadata)? {
        return Ok(local_path);
    }
    if let Some(local_path) = find_local_platform_config_path(platform_package, deps_metadata)? {
        return Ok(local_path);
    }

    bail!(
        "failed to resolve platform config for `{}`. Ensure the platform crate is a workspace \
         member or dependency and contains an axconfig.toml next to its Cargo.toml",
        platform_package
    );
}

pub(super) fn find_local_platform_config_path(
    platform_package: &str,
    metadata: &Metadata,
) -> anyhow::Result<Option<PathBuf>> {
    if let Some(candidate) = platform_config_path_from_metadata(platform_package, metadata) {
        return Ok(Some(candidate));
    }

    if let Some(pkg) = metadata_package(metadata, platform_package) {
        let candidate = Path::new(pkg.manifest_path.as_std_path())
            .parent()
            .map(|dir| dir.join("axconfig.toml"));
        if let Some(candidate) = candidate
            && candidate.exists()
        {
            return Ok(Some(candidate));
        }
    }

    let workspace_root = crate::context::workspace_root_path()?;
    let platform_candidate = workspace_root
        .join("platforms")
        .join(platform_package)
        .join("axconfig.toml");

    Ok(platform_candidate.exists().then_some(platform_candidate))
}

pub(super) fn read_platform_name(platform_config: &Path) -> Option<String> {
    read_config_string(&[platform_config.to_path_buf()], "platform").ok()
}

pub(crate) fn generate_axconfig(
    workspace_root: &Path,
    target: &str,
    platform_name: &str,
    platform_config: &Path,
    out_config: &Path,
    max_cpu_num: Option<usize>,
    axconfig_overrides: &[String],
) -> anyhow::Result<()> {
    let defconfig = resolve_defconfig_path(workspace_root)?;
    if let Some(parent) = out_config.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create OUT_CONFIG parent directory {}",
                parent.display()
            )
        })?;
    }

    let arch = target_arch_name(target)?;
    let mut writes = vec![
        format!("arch=\"{arch}\""),
        format!("platform=\"{platform_name}\""),
    ];
    if let Some(max_cpu_num) = max_cpu_num {
        writes.push(format!("plat.max-cpu-num={max_cpu_num}"));
    }
    writes.extend(axconfig_overrides.iter().cloned());

    generate_config(&GenerateOptions {
        specs: vec![defconfig, platform_config.to_path_buf()],
        oldconfig: None,
        output: Some(out_config.to_path_buf()),
        fmt: ax_config_gen::OutputFormat::Toml,
        writes,
        keep_backup: false,
    })
    .context("failed to generate axconfig")?;

    Ok(())
}

pub(super) fn resolve_defconfig_path(workspace_root: &Path) -> anyhow::Result<PathBuf> {
    let path = workspace_root.join("os/arceos/configs/defconfig.toml");
    if path.exists() {
        Ok(path)
    } else {
        Err(anyhow::anyhow!(
            "defconfig.toml not found at {}",
            path.display()
        ))
    }
}
