use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, anyhow, bail};
use cargo_metadata::{Metadata, Package};
use log::{info, warn};
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    context::{axbuild_tmp_dir, workspace_manifest_path, workspace_metadata_root_manifest},
    support::process::ProcessExt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AxFeaturePrefixFamily {
    AxStd,
    AxFeat,
}

impl AxFeaturePrefixFamily {
    fn prefix(self) -> &'static str {
        match self {
            Self::AxStd => "ax-std/",
            Self::AxFeat => "ax-feat/",
        }
    }
}

#[derive(Debug, Clone, JsonSchema, Deserialize, Serialize, PartialEq)]
pub struct BuildInfo {
    /// Environment variables to set during the build.
    pub env: HashMap<String, String>,
    /// Cargo features to enable.
    pub features: Vec<String>,
    /// Log level feature to automatically enable.
    pub log: LogLevel,
    /// Maximum number of CPUs to expose to the build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpu_num: Option<usize>,
    /// Additional `ax-config-gen -w` overrides applied when generating `.axconfig.toml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axconfig_overrides: Vec<String>,
    /// Whether to use the dynamic platform linker flow when supported.
    #[serde(default, skip_serializing_if = "is_false")]
    pub plat_dyn: bool,
}

impl BuildInfo {
    pub fn with_features<T: AsRef<str>>(mut self, features: impl AsRef<[T]>) -> Self {
        let features = features
            .as_ref()
            .iter()
            .map(|feature| feature.as_ref().to_string())
            .collect();
        self.features = features;
        self
    }

    pub fn default_for_target(target: &str) -> Self {
        Self {
            plat_dyn: supports_platform_dynamic(target),
            ..Self::default()
        }
    }

    pub(crate) fn effective_plat_dyn(&self, target: &str, plat_dyn_override: Option<bool>) -> bool {
        resolve_effective_plat_dyn(target, self.plat_dyn, plat_dyn_override)
    }

    pub(crate) fn prepare_log_env(&mut self) {
        self.env
            .insert("AX_LOG".into(), format!("{:?}", self.log).to_lowercase());
    }

    pub(crate) fn prepare_max_cpu_num_env(&mut self) -> anyhow::Result<()> {
        if let Some(max_cpu_num) = self.validated_max_cpu_num()? {
            self.env.insert("SMP".into(), max_cpu_num.to_string());
        }
        Ok(())
    }

    pub(crate) fn into_base_cargo_config(
        self,
        package: String,
        target: String,
        args: Vec<String>,
    ) -> Cargo {
        let to_bin = default_to_bin_for_target(&target);
        Cargo {
            env: self.env,
            target,
            package,
            features: self.features,
            log: Some(self.log),
            extra_config: None,
            args,
            pre_build_cmds: vec![],
            post_build_cmds: vec![],
            to_bin,
        }
    }

    pub(crate) fn into_base_cargo_config_with_log(
        mut self,
        package: String,
        target: String,
        args: Vec<String>,
    ) -> Cargo {
        self.prepare_log_env();
        self.prepare_max_cpu_num_env()
            .expect("max_cpu_num validation should run before cargo config generation");
        self.into_base_cargo_config(package, target, args)
    }

    pub(crate) fn into_prepared_base_cargo_config(
        self,
        package: &str,
        target: &str,
        plat_dyn_override: Option<bool>,
    ) -> anyhow::Result<Cargo> {
        let metadata = workspace_metadata().context("failed to load workspace metadata")?;
        self.into_prepared_base_cargo_config_with_metadata(
            package,
            target,
            plat_dyn_override,
            &metadata,
        )
    }

    pub(crate) fn into_prepared_base_cargo_config_with_metadata(
        mut self,
        package: &str,
        target: &str,
        plat_dyn_override: Option<bool>,
        metadata: &Metadata,
    ) -> anyhow::Result<Cargo> {
        let plat_dyn = self.effective_plat_dyn(target, plat_dyn_override);
        self.validated_max_cpu_num()?;
        self.prepare_non_dynamic_platform_for(package, target, plat_dyn, metadata)?;
        self.resolve_features_with_metadata(package, plat_dyn, metadata);
        let args = Self::build_cargo_args(target, plat_dyn);

        Ok(self.into_base_cargo_config_with_log(package.to_string(), target.to_string(), args))
    }

    pub(crate) fn prepare_non_dynamic_platform_for(
        &mut self,
        package: &str,
        target: &str,
        plat_dyn: bool,
        metadata: &Metadata,
    ) -> anyhow::Result<()> {
        if plat_dyn {
            return Ok(());
        }

        ensure_arceos_tooling_installed()?;

        let package_manifest = resolve_package_manifest_path(package, metadata)?;
        let app_dir = package_manifest
            .parent()
            .context("package manifest path has no parent directory")?;
        let platform_package = resolve_platform_package(package, target, &self.features, metadata)?;
        let platform_config = resolve_platform_config_path(app_dir, &platform_package, metadata)?;
        let platform_name = read_platform_name(&platform_config)
            .unwrap_or_else(|| linker_platform_name(&platform_package).to_string());
        let out_config = generated_axconfig_path(package, target)?;

        generate_axconfig(
            &crate::context::workspace_root_path()?,
            target,
            &platform_name,
            &platform_config,
            &out_config,
            self.validated_max_cpu_num()?,
            &self.axconfig_overrides,
        )?;

        self.env.insert(
            "AX_CONFIG_PATH".to_string(),
            out_config.display().to_string(),
        );
        self.env
            .insert("AX_PLATFORM".to_string(), platform_name.to_string());

        Ok(())
    }

    pub(crate) fn resolve_features_with_metadata(
        &mut self,
        package: &str,
        plat_dyn: bool,
        metadata: &Metadata,
    ) {
        self.resolve_features_with_prefix_family(
            package,
            plat_dyn,
            detect_ax_feature_prefix_family(package, metadata),
        );
    }

    fn resolve_features_with_prefix_family(
        &mut self,
        package: &str,
        plat_dyn: bool,
        prefix_family: anyhow::Result<AxFeaturePrefixFamily>,
    ) {
        let prefix_family = self.resolve_ax_feature_prefix_family(package, prefix_family);
        let has_myplat = self.features.iter().any(|feature| {
            matches!(
                feature.as_str(),
                "myplat" | "ax-std/myplat" | "ax-feat/myplat"
            )
        });

        self.features.retain(|feature| {
            !matches!(
                feature.as_str(),
                "plat-dyn"
                    | "defplat"
                    | "myplat"
                    | "ax-std/plat-dyn"
                    | "ax-std/defplat"
                    | "ax-std/myplat"
                    | "ax-feat/plat-dyn"
                    | "ax-feat/defplat"
                    | "ax-feat/myplat"
            )
        });

        if plat_dyn {
            self.features
                .push(format!("{}plat-dyn", prefix_family.prefix()));
        } else if has_myplat {
            self.features
                .push(format!("{}myplat", prefix_family.prefix()));
        } else {
            self.features
                .push(format!("{}defplat", prefix_family.prefix()));
        }

        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push(format!("{}smp", prefix_family.prefix()));
        }

        self.features.sort();
        self.features.dedup();
    }

    fn resolve_ax_feature_prefix_family(
        &self,
        package: &str,
        prefix_family: anyhow::Result<AxFeaturePrefixFamily>,
    ) -> AxFeaturePrefixFamily {
        match prefix_family {
            Ok(prefix_family) => prefix_family,
            Err(err) => {
                if let Some(prefix_family) = feature_family_from_existing_features(&self.features) {
                    return prefix_family;
                }
                warn!(
                    "failed to detect direct ax dependency for package {}: {}, defaulting to \
                     ax-std feature prefix",
                    package, err
                );
                AxFeaturePrefixFamily::AxStd
            }
        }
    }

    pub(crate) fn normalize_legacy_feature_aliases(&mut self) -> bool {
        let mut changed = false;

        for feature in &mut self.features {
            let normalized = normalize_legacy_feature_alias(feature);
            if *feature != normalized {
                *feature = normalized;
                changed = true;
            }
        }

        if changed {
            self.features.sort();
            self.features.dedup();
        }

        changed
    }

    #[cfg(test)]
    pub(crate) fn resolve_features(&mut self, package: &str, plat_dyn: bool) {
        match workspace_metadata() {
            Ok(metadata) => self.resolve_features_with_metadata(package, plat_dyn, &metadata),
            Err(err) => self.resolve_features_with_prefix_family(
                package,
                plat_dyn,
                Err(err.context("failed to load workspace metadata")),
            ),
        }
    }

    pub(crate) fn validated_max_cpu_num(&self) -> anyhow::Result<Option<usize>> {
        match self.max_cpu_num {
            Some(0) => bail!("max_cpu_num must be greater than 0"),
            Some(max_cpu_num) => Ok(Some(max_cpu_num)),
            None => Ok(None),
        }
    }

    pub(crate) fn build_cargo_args(target: &str, plat_dyn: bool) -> Vec<String> {
        let mut args = Vec::new();
        args.push("--config".to_string());
        args.push(if plat_dyn {
            format!("target.{target}.rustflags=[\"-Clink-arg=-Taxplat.x\"]")
        } else {
            format!(
                "target.{target}.rustflags=[\"-Clink-arg=-Tlinker.x\",\"-Clink-arg=-no-pie\",\"\
                 -Clink-arg=-znostart-stop-gc\"]"
            )
        });
        args
    }
}

impl Default for BuildInfo {
    fn default() -> Self {
        let mut env = HashMap::new();
        env.insert("AX_IP".to_string(), "10.0.2.15".to_string());
        env.insert("AX_GW".to_string(), "10.0.2.2".to_string());

        Self {
            env,
            log: LogLevel::Warn,
            features: vec!["ax-std".to_string()],
            max_cpu_num: None,
            axconfig_overrides: Vec::new(),
            plat_dyn: false,
        }
    }
}

pub(crate) fn load_or_create_build_info<T>(
    path: &Path,
    default: impl FnOnce() -> T,
) -> anyhow::Result<T>
where
    T: Serialize + DeserializeOwned,
{
    println!("Using build config: {}", path.display());

    if path.exists() {
        info!("Found build config at {}", path.display());
    } else {
        info!(
            "Build config not found at {}, writing default config",
            path.display()
        );
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let default = default();
        std::fs::write(path, toml::to_string_pretty(&default)?)?;
    }

    toml::from_str::<T>(&std::fs::read_to_string(path)?)
        .with_context(|| format!("failed to parse build info {}", path.display()))
}

fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn resolve_effective_plat_dyn(
    target: &str,
    configured_plat_dyn: bool,
    plat_dyn_override: Option<bool>,
) -> bool {
    plat_dyn_override.unwrap_or(configured_plat_dyn) && supports_platform_dynamic(target)
}

fn supports_platform_dynamic(target: &str) -> bool {
    target.starts_with("aarch64-")
}

fn default_to_bin_for_target(target: &str) -> bool {
    !target.starts_with("x86_64-")
}

fn normalize_legacy_feature_alias(feature: &str) -> String {
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
    package: &str,
    makefile_features: &[String],
) {
    if makefile_features.is_empty() {
        return;
    }
    let prefix_family = workspace_metadata()
        .and_then(|metadata| detect_ax_feature_prefix_family(package, &metadata))
        .map_err(|err| err.context("failed to load workspace metadata"));
    apply_makefile_features_with_prefix_family(
        build_info,
        package,
        makefile_features,
        prefix_family,
    );
}

pub(crate) fn apply_makefile_features_with_metadata(
    build_info: &mut BuildInfo,
    package: &str,
    makefile_features: &[String],
    metadata: &Metadata,
) {
    apply_makefile_features_with_prefix_family(
        build_info,
        package,
        makefile_features,
        detect_ax_feature_prefix_family(package, metadata),
    );
}

fn apply_makefile_features_with_prefix_family(
    build_info: &mut BuildInfo,
    package: &str,
    makefile_features: &[String],
    prefix_family: anyhow::Result<AxFeaturePrefixFamily>,
) {
    if makefile_features.is_empty() {
        return;
    }

    let prefix_family = build_info.resolve_ax_feature_prefix_family(package, prefix_family);

    for feature in makefile_features {
        let normalized = normalize_legacy_feature_alias(feature);
        let mapped =
            if normalized.contains('/') || matches!(normalized.as_str(), "ax-std" | "ax-feat") {
                normalized
            } else {
                format!("{}{}", prefix_family.prefix(), normalized)
            };

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

fn generated_axconfig_path(package: &str, target: &str) -> anyhow::Result<PathBuf> {
    Ok(axbuild_tmp_dir(&crate::context::workspace_root_path()?)
        .join("axconfig")
        .join(package)
        .join(target)
        .join(".axconfig.toml"))
}

fn feature_family_from_existing_features(features: &[String]) -> Option<AxFeaturePrefixFamily> {
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

fn workspace_package<'a>(metadata: &'a Metadata, package: &str) -> anyhow::Result<&'a Package> {
    metadata
        .packages
        .iter()
        .find(|pkg| metadata.workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))
}

fn resolve_package_manifest_path(package: &str, metadata: &Metadata) -> anyhow::Result<PathBuf> {
    workspace_package(metadata, package).map(|pkg| pkg.manifest_path.clone().into_std_path_buf())
}

fn detect_ax_feature_prefix_family(
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

fn resolve_platform_package(
    package: &str,
    target: &str,
    features: &[String],
    metadata: &Metadata,
) -> anyhow::Result<String> {
    let arch = target_arch_name(target)?;
    let package_info = workspace_package(metadata, package)?;

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

    if let Some(dep) = package_info.dependencies.iter().find(|dep| {
        (dep.name.starts_with("axplat-") || dep.name.starts_with("ax-plat-"))
            && explicit_platform_features
                .iter()
                .any(|feature| *feature == linker_platform_name(&dep.name))
    }) {
        return Ok(dep.name.clone());
    }

    if features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "myplat" | "ax-std/myplat" | "ax-feat/myplat"
        )
    }) {
        if let Some(dep_name) = explicit_myplat_platform_package(package, arch)
            && package_info
                .dependencies
                .iter()
                .any(|dep| dep.name == dep_name)
        {
            return Ok(dep_name.to_string());
        }

        if let Some(dep) = package_info
            .dependencies
            .iter()
            .find(|dep| myplat_dependency_matches_arch(&dep.name, arch))
        {
            return Ok(dep.name.clone());
        }
    }

    Ok(default_platform_package(arch).to_string())
}

fn target_arch_name(target: &str) -> anyhow::Result<&'static str> {
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

fn default_platform_package(arch: &str) -> &'static str {
    match arch {
        "x86_64" => "ax-plat-x86-pc",
        "aarch64" => "ax-plat-aarch64-qemu-virt",
        "riscv64" => "ax-plat-riscv64-qemu-virt",
        "loongarch64" => "ax-plat-loongarch64-qemu-virt",
        _ => unreachable!("unsupported arch"),
    }
}

fn explicit_myplat_platform_package(package: &str, arch: &str) -> Option<&'static str> {
    match (package, arch) {
        ("axvisor", "x86_64") => Some("axplat-x86-qemu-q35"),
        ("axvisor", "riscv64") => Some("axplat-riscv64-qemu-virt-hv"),
        _ => None,
    }
}

fn myplat_dependency_matches_arch(dep_name: &str, arch: &str) -> bool {
    myplat_dependency_prefixes_for_arch(arch)
        .iter()
        .any(|prefix| dep_name.starts_with(prefix))
}

fn myplat_dependency_prefixes_for_arch(arch: &str) -> &'static [&'static str] {
    match arch {
        "x86_64" => &["axplat-x86-", "axplat-x86_64-"],
        "aarch64" => &["axplat-aarch64-"],
        "riscv64" => &["axplat-riscv64-"],
        "loongarch64" => &["axplat-loongarch64-"],
        _ => &[],
    }
}

fn linker_platform_name(platform_package: &str) -> &str {
    platform_package
        .strip_prefix("axplat-")
        .or_else(|| platform_package.strip_prefix("ax-plat-"))
        .unwrap_or(platform_package)
}

fn resolve_platform_config_path(
    app_dir: &Path,
    platform_package: &str,
    metadata: &Metadata,
) -> anyhow::Result<PathBuf> {
    if let Some(local_path) = find_local_platform_config_path(platform_package, metadata)? {
        return Ok(local_path);
    }

    let workspace_root = crate::context::workspace_root_path()?;
    let root_manifest = workspace_root.join("Cargo.toml");
    let output = Command::new("cargo")
        .arg("axplat")
        .arg("info")
        .arg("--manifest-path")
        .arg(&root_manifest)
        .arg("-C")
        .arg(app_dir)
        .arg("-c")
        .arg(platform_package)
        .exec_capture()
        .with_context(|| format!("failed to run cargo axplat info for `{platform_package}`"))?;

    let config_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if config_path.is_empty() {
        bail!(
            "cargo axplat info returned empty config path for package `{}`",
            platform_package
        );
    }

    let config_path = PathBuf::from(config_path);
    if !config_path.exists() {
        bail!(
            "platform config path does not exist: {}",
            config_path.display()
        );
    }

    Ok(config_path)
}

fn find_local_platform_config_path(
    platform_package: &str,
    metadata: &Metadata,
) -> anyhow::Result<Option<PathBuf>> {
    if let Ok(pkg) = workspace_package(metadata, platform_package) {
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
    let platform_dir_name = platform_package
        .strip_prefix("ax-plat-")
        .map(|suffix| format!("axplat-{suffix}"))
        .unwrap_or_else(|| platform_package.to_string());
    let component_candidate = workspace_root
        .join("components/axplat_crates/platforms")
        .join(platform_dir_name)
        .join("axconfig.toml");

    Ok(component_candidate.exists().then_some(component_candidate))
}

fn ensure_arceos_tooling_installed() -> anyhow::Result<()> {
    ensure_cargo_axplat_installed()?;
    ensure_ax_config_gen_installed()?;
    Ok(())
}

fn ensure_cargo_axplat_installed() -> anyhow::Result<()> {
    if Command::new("cargo")
        .arg("axplat")
        .arg("--version")
        .exec_capture()
        .is_ok()
    {
        return Ok(());
    }

    if std::env::var("AXBUILD_AUTO_INSTALL_TOOLS").as_deref() != Ok("1") {
        bail!(
            "`cargo axplat` is not installed.\nInstall it manually with: cargo install \
             cargo-axplat\nOr set AXBUILD_AUTO_INSTALL_TOOLS=1 to allow automatic installation."
        );
    }
    warn!("`cargo axplat` not found, installing `cargo-axplat` via cargo");
    Command::new("cargo")
        .arg("install")
        .arg("cargo-axplat")
        .exec()
        .context("failed to install cargo-axplat")?;
    Ok(())
}

fn ensure_ax_config_gen_installed() -> anyhow::Result<()> {
    if Command::new("ax-config-gen")
        .arg("--version")
        .exec_capture()
        .is_ok()
    {
        return Ok(());
    }

    let workspace_root = crate::context::workspace_root_path()?;
    let ax_config_gen_dir = workspace_root.join("components/axconfig-gen/axconfig-gen");

    if std::env::var("AXBUILD_AUTO_INSTALL_TOOLS").as_deref() != Ok("1") {
        bail!(
            "`ax-config-gen` is not installed.\nInstall it manually with: cargo install --path \
             {}\nOr set AXBUILD_AUTO_INSTALL_TOOLS=1 to allow automatic installation.",
            ax_config_gen_dir.display()
        );
    }

    warn!(
        "`ax-config-gen` not found, installing from local path {}",
        ax_config_gen_dir.display()
    );
    Command::new("cargo")
        .arg("install")
        .arg("--path")
        .arg(&ax_config_gen_dir)
        .exec()
        .with_context(|| {
            format!(
                "failed to install ax-config-gen from {}",
                ax_config_gen_dir.display()
            )
        })?;
    Ok(())
}

fn read_platform_name(platform_config: &Path) -> Option<String> {
    let contents = fs::read_to_string(platform_config).ok()?;
    let value: toml::Value = toml::from_str(&contents).ok()?;
    value
        .get("platform")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

fn generate_axconfig(
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
    let mut command = Command::new("ax-config-gen");
    command
        .arg(defconfig)
        .arg(platform_config)
        .arg("-w")
        .arg(format!("arch=\"{arch}\""))
        .arg("-w")
        .arg(format!("platform=\"{platform_name}\""));
    if let Some(max_cpu_num) = max_cpu_num {
        command
            .arg("-w")
            .arg(format!("plat.max-cpu-num={max_cpu_num}"));
    }
    for override_value in axconfig_overrides {
        command.arg("-w").arg(override_value);
    }
    command
        .arg("-o")
        .arg(out_config)
        .exec()
        .context("failed to run ax-config-gen")?;

    Ok(())
}

fn resolve_defconfig_path(workspace_root: &Path) -> anyhow::Result<PathBuf> {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn metadata_for_manifest(manifest_path: &Path) -> cargo_metadata::Metadata {
        workspace_metadata_root_manifest(manifest_path).unwrap()
    }

    fn repo_metadata() -> cargo_metadata::Metadata {
        workspace_metadata().unwrap()
    }

    fn temp_workspace(
        package_name: &str,
        dependency_block: &str,
    ) -> anyhow::Result<std::path::PathBuf> {
        let root = tempdir()?.keep();

        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n\n[workspace.package]\nedition = \
             \"2024\"\n",
        )?;

        let app_dir = root.join("app");
        fs::create_dir_all(&app_dir)?;
        fs::write(
            app_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \
                 \"2024\"\n\n[dependencies]\n{dependency_block}"
            ),
        )?;
        fs::create_dir_all(app_dir.join("src"))?;
        fs::write(app_dir.join("src/lib.rs"), "pub fn smoke() {}\n")?;

        Ok(root)
    }

    #[test]
    fn detects_axfeat_direct_dependency_via_metadata() {
        let workspace = temp_workspace("ax-feat-app", "ax-feat = \"0.1.0\"\n").unwrap();

        let metadata = metadata_for_manifest(&workspace.join("Cargo.toml"));
        let family = detect_ax_feature_prefix_family("ax-feat-app", &metadata).unwrap();

        assert_eq!(family, AxFeaturePrefixFamily::AxFeat);
    }

    #[test]
    fn resolve_platform_package_prefers_matching_explicit_platform_dependency() {
        let metadata = repo_metadata();
        let platform = resolve_platform_package(
            "ax-helloworld-myplat",
            "aarch64-unknown-none-softfloat",
            &["aarch64-qemu-virt".to_string()],
            &metadata,
        )
        .unwrap();

        assert_eq!(platform, "ax-plat-aarch64-qemu-virt");
    }

    #[test]
    fn find_local_platform_config_path_resolves_workspace_platform_dir() {
        let metadata = repo_metadata();
        let path = find_local_platform_config_path("axplat-riscv64-qemu-virt-hv", &metadata)
            .unwrap()
            .expect("workspace platform config should exist");

        assert!(path.ends_with("platform/riscv64-qemu-virt/axconfig.toml"));
    }
}
