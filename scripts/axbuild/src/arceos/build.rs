use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    context::{ResolvedBuildRequest, workspace_manifest_path, workspace_metadata_root_manifest},
    process::ProcessExt,
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
pub struct ArceosBuildInfo {
    /// Environment variables to set during the build.
    pub env: HashMap<String, String>,
    /// Cargo features to enable.
    pub features: Vec<String>,
    /// Log level feature to automatically enable.
    pub log: LogLevel,
    /// Maximum number of CPUs to expose to the build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpu_num: Option<usize>,
    /// Whether to use the dynamic platform linker flow when supported.
    #[serde(default, skip_serializing_if = "is_false")]
    pub plat_dyn: bool,
}

impl ArceosBuildInfo {
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

    pub(crate) fn resolve_features(&mut self, package: &str, plat_dyn: bool) {
        self.resolve_features_with_manifest_path(package, plat_dyn, None);
    }

    fn resolve_features_with_manifest_path(
        &mut self,
        package: &str,
        plat_dyn: bool,
        manifest_path: Option<&Path>,
    ) {
        let prefix_family = self.resolve_ax_feature_prefix_family(package, manifest_path);
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
        manifest_path: Option<&Path>,
    ) -> AxFeaturePrefixFamily {
        match detect_ax_feature_prefix_family(package, manifest_path) {
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

    fn normalize_legacy_feature_aliases(&mut self) -> bool {
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
        mut self,
        package: &str,
        target: &str,
        plat_dyn_override: Option<bool>,
    ) -> anyhow::Result<Cargo> {
        let plat_dyn = self.effective_plat_dyn(target, plat_dyn_override);
        self.validated_max_cpu_num()?;
        self.prepare_non_dynamic_platform_for(package, target, plat_dyn)?;
        self.resolve_features(package, plat_dyn);
        let args = Self::build_cargo_args(target, plat_dyn);

        Ok(self.into_base_cargo_config_with_log(package.to_string(), target.to_string(), args))
    }

    pub(crate) fn prepare_non_dynamic_platform_for(
        &mut self,
        package: &str,
        target: &str,
        plat_dyn: bool,
    ) -> anyhow::Result<()> {
        if plat_dyn {
            return Ok(());
        }

        ensure_arceos_tooling_installed()?;

        let package_manifest = resolve_package_manifest_path(package, None)?;
        let app_dir = package_manifest
            .parent()
            .context("package manifest path has no parent directory")?;
        let platform_package = resolve_platform_package(package, target, &self.features)?;
        let platform_config = resolve_platform_config_path(app_dir, &platform_package)?;
        let platform_name = read_platform_name(&platform_config)
            .unwrap_or_else(|| linker_platform_name(&platform_package).to_string());
        let out_config = app_dir.join(".axconfig.toml");

        generate_axconfig(
            &workspace_root_path()?,
            target,
            &platform_name,
            &platform_config,
            &out_config,
            self.validated_max_cpu_num()?,
        )?;

        self.env.insert(
            "AX_CONFIG_PATH".to_string(),
            out_config.display().to_string(),
        );
        self.env
            .insert("AX_PLATFORM".to_string(), platform_name.to_string());

        Ok(())
    }

    fn validated_max_cpu_num(&self) -> anyhow::Result<Option<usize>> {
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

    pub fn into_cargo_config(self, request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
        self.into_prepared_base_cargo_config(&request.package, &request.target, request.plat_dyn)
    }
}

impl Default for ArceosBuildInfo {
    fn default() -> Self {
        let mut env = HashMap::new();
        env.insert("AX_IP".to_string(), "10.0.2.15".to_string());
        env.insert("AX_GW".to_string(), "10.0.2.2".to_string());

        Self {
            env,
            log: LogLevel::Warn,
            features: vec!["ax-std".to_string()],
            max_cpu_num: None,
            plat_dyn: false,
        }
    }
}

pub(crate) fn resolve_build_info_path(
    package: &str,
    target: &str,
    explicit_path: Option<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(path) = explicit_path {
        return Ok(path);
    }

    let package_manifest = resolve_package_manifest_path(package, None)?;
    let app_dir = package_manifest
        .parent()
        .context("package manifest path has no parent directory")?;
    Ok(resolve_build_info_path_in_dir(app_dir, target))
}

pub(crate) fn load_build_info(request: &ResolvedBuildRequest) -> anyhow::Result<ArceosBuildInfo> {
    let makefile_features = makefile_features_from_env();
    load_build_info_with_makefile_features(request, &makefile_features)
}

pub(crate) fn load_build_info_with_makefile_features(
    request: &ResolvedBuildRequest,
    makefile_features: &[String],
) -> anyhow::Result<ArceosBuildInfo> {
    let mut build_info = load_or_create_build_info(&request.build_info_path, || {
        ArceosBuildInfo::default_for_target(&request.target)
    })?;

    if build_info.normalize_legacy_feature_aliases() {
        warn!(
            "normalizing legacy feature aliases in build config {}",
            request.build_info_path.display()
        );
        fs::write(
            &request.build_info_path,
            toml::to_string_pretty(&build_info)?,
        )
        .with_context(|| {
            format!(
                "failed to rewrite normalized build info {}",
                request.build_info_path.display()
            )
        })?;
    }

    apply_makefile_features(&mut build_info, &request.package, makefile_features);

    if let Some(smp) = request.smp {
        build_info.max_cpu_num = Some(smp);
    }

    Ok(build_info)
}

pub(crate) fn load_cargo_config(request: &ResolvedBuildRequest) -> anyhow::Result<Cargo> {
    load_build_info(request)?.into_cargo_config(request)
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

fn resolve_effective_plat_dyn(
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

fn is_false(value: &bool) -> bool {
    !*value
}

pub(crate) fn normalize_legacy_feature_alias(feature: &str) -> String {
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
    build_info: &mut ArceosBuildInfo,
    package: &str,
    makefile_features: &[String],
) {
    apply_makefile_features_with_manifest_path(build_info, package, makefile_features, None);
}

fn apply_makefile_features_with_manifest_path(
    build_info: &mut ArceosBuildInfo,
    package: &str,
    makefile_features: &[String],
    manifest_path: Option<&Path>,
) {
    if makefile_features.is_empty() {
        return;
    }

    let prefix_family = build_info.resolve_ax_feature_prefix_family(package, manifest_path);

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

pub(crate) fn resolve_build_info_path_in_dir(dir: &Path, target: &str) -> PathBuf {
    let bare_path = dir.join(format!("build-{target}.toml"));
    if bare_path.exists() {
        return bare_path;
    }

    let dotted_path = dir.join(format!(".build-{target}.toml"));
    if dotted_path.exists() {
        return dotted_path;
    }

    dotted_path
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

fn detect_ax_feature_prefix_family(
    package: &str,
    workspace_manifest: Option<&Path>,
) -> anyhow::Result<AxFeaturePrefixFamily> {
    let manifest_path = workspace_manifest
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(workspace_manifest_path)?;
    let metadata = workspace_metadata_root_manifest(&manifest_path)?;
    let workspace_members: std::collections::HashSet<_> =
        metadata.workspace_members.iter().cloned().collect();
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))?;

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

pub(crate) fn resolve_package_manifest_path(
    package: &str,
    workspace_manifest: Option<&Path>,
) -> anyhow::Result<PathBuf> {
    let manifest_path = workspace_manifest
        .map(Path::to_path_buf)
        .map(Ok)
        .unwrap_or_else(workspace_manifest_path)?;
    let metadata = workspace_metadata_root_manifest(&manifest_path)?;
    let workspace_members: std::collections::HashSet<_> =
        metadata.workspace_members.iter().cloned().collect();
    metadata
        .packages
        .iter()
        .find(|pkg| workspace_members.contains(&pkg.id) && pkg.name == package)
        .map(|pkg| pkg.manifest_path.clone().into_std_path_buf())
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))
}

fn resolve_platform_package(
    package: &str,
    target: &str,
    features: &[String],
) -> anyhow::Result<String> {
    let arch = target_arch_name(target)?;
    let workspace_manifest = workspace_manifest_path()?;
    let metadata = workspace_metadata_root_manifest(&workspace_manifest)?;
    let package_info = metadata
        .packages
        .iter()
        .find(|pkg| metadata.workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow!("workspace package `{package}` not found"))?;

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

fn resolve_platform_config_path(app_dir: &Path, platform_package: &str) -> anyhow::Result<PathBuf> {
    if let Some(local_path) = find_local_platform_config_path(platform_package)? {
        return Ok(local_path);
    }

    let workspace_root = workspace_root_path()?;
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

fn find_local_platform_config_path(platform_package: &str) -> anyhow::Result<Option<PathBuf>> {
    let workspace_manifest = workspace_manifest_path()?;
    let metadata = workspace_metadata_root_manifest(&workspace_manifest)?;

    if let Some(pkg) = metadata
        .packages
        .iter()
        .find(|pkg| metadata.workspace_members.contains(&pkg.id) && pkg.name == platform_package)
    {
        let candidate = Path::new(pkg.manifest_path.as_std_path())
            .parent()
            .map(|dir| dir.join("axconfig.toml"));
        if let Some(candidate) = candidate
            && candidate.exists()
        {
            return Ok(Some(candidate));
        }
    }

    let workspace_root = workspace_root_path()?;
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

    let workspace_root = workspace_root_path()?;
    let ax_config_gen_dir = workspace_root.join("components/axconfig-gen/axconfig-gen");

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
) -> anyhow::Result<()> {
    let defconfig = resolve_defconfig_path(workspace_root)?;
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
    command
        .arg("-o")
        .arg(out_config)
        .exec()
        .context("failed to run ax-config-gen")?;

    Ok(())
}

fn workspace_root_path() -> anyhow::Result<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .context("failed to locate workspace root from axbuild crate")?;
    Ok(root.to_path_buf())
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

    fn request(
        package: &str,
        target: &str,
        plat_dyn: Option<bool>,
        build_info_path: PathBuf,
    ) -> ResolvedBuildRequest {
        ResolvedBuildRequest {
            package: package.to_string(),
            arch: if target.starts_with("x86_64") {
                "x86_64".to_string()
            } else if target.starts_with("aarch64") {
                "aarch64".to_string()
            } else if target.starts_with("riscv64") {
                "riscv64".to_string()
            } else if target.starts_with("loongarch64") {
                "loongarch64".to_string()
            } else {
                "unknown".to_string()
            },
            target: target.to_string(),
            plat_dyn,
            smp: None,
            debug: false,
            build_info_path,
            qemu_config: None,
            uboot_config: None,
        }
    }

    fn base_build_info() -> ArceosBuildInfo {
        ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat")
    }

    #[test]
    fn resolves_dynamic_platform_features_and_args() {
        let mut build_info = base_build_info();
        build_info.resolve_features("ax-helloworld", true);

        assert!(build_info.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(!build_info.features.contains(&"ax-std/defplat".to_string()));

        let args = ArceosBuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", true);
        assert!(args.iter().any(|arg| arg.contains("-Taxplat.x")));
    }

    #[test]
    fn resolves_non_dynamic_platform_features_and_args() {
        let mut build_info = base_build_info();
        build_info.resolve_features("ax-helloworld", false);

        assert!(build_info.features.contains(&"ax-std/defplat".to_string()));
        assert!(!build_info.features.contains(&"ax-std/plat-dyn".to_string()));

        let args = ArceosBuildInfo::build_cargo_args("aarch64-unknown-none-softfloat", false);
        assert!(args.iter().any(|arg| arg.contains("-Tlinker.x")));
    }

    #[test]
    fn max_cpu_num_adds_axstd_smp_feature() {
        let mut build_info = ArceosBuildInfo {
            max_cpu_num: Some(4),
            ..ArceosBuildInfo::default()
        };

        build_info.resolve_features("ax-helloworld", false);

        assert!(build_info.features.contains(&"ax-std/smp".to_string()));
    }

    #[test]
    fn preserves_axstd_myplat_for_non_dynamic_platforms() {
        let mut build_info = ArceosBuildInfo {
            features: vec!["ax-std".to_string(), "ax-std/myplat".to_string()],
            ..ArceosBuildInfo::default()
        };
        build_info.resolve_features("ax-helloworld", false);

        assert!(build_info.features.contains(&"ax-std/myplat".to_string()));
        assert!(!build_info.features.contains(&"ax-std/defplat".to_string()));
    }

    #[test]
    fn normalizes_myplat_to_axfeat_when_package_depends_on_axfeat() {
        let workspace = temp_workspace("ax-feat-app", "ax-feat = \"0.1.0\"\n").unwrap();
        let mut build_info = ArceosBuildInfo {
            features: vec!["ax-std/myplat".to_string()],
            ..ArceosBuildInfo::default()
        };

        let family =
            detect_ax_feature_prefix_family("ax-feat-app", Some(&workspace.join("Cargo.toml")))
                .unwrap();
        assert_eq!(family, AxFeaturePrefixFamily::AxFeat);

        build_info.features.retain(|feature| feature != "ax-std");
        build_info.resolve_features_with_manifest_path(
            "ax-feat-app",
            false,
            Some(&workspace.join("Cargo.toml")),
        );

        assert!(build_info.features.contains(&"ax-feat/myplat".to_string()));
        assert!(!build_info.features.contains(&"ax-std/myplat".to_string()));
        assert!(!build_info.features.contains(&"ax-feat/defplat".to_string()));
    }

    #[test]
    fn detects_axfeat_direct_dependency_via_metadata() {
        let workspace = temp_workspace("ax-feat-app", "ax-feat = \"0.1.0\"\n").unwrap();

        let family =
            detect_ax_feature_prefix_family("ax-feat-app", Some(&workspace.join("Cargo.toml")))
                .unwrap();

        assert_eq!(family, AxFeaturePrefixFamily::AxFeat);
    }

    #[test]
    fn max_cpu_num_adds_axfeat_smp_feature() {
        let workspace = temp_workspace("ax-feat-app", "ax-feat = \"0.1.0\"\n").unwrap();
        let mut build_info = ArceosBuildInfo {
            features: vec!["ax-feat/net".to_string()],
            max_cpu_num: Some(4),
            ..ArceosBuildInfo::default()
        };

        build_info.features.retain(|feature| feature != "ax-std");
        build_info.resolve_features_with_manifest_path(
            "ax-feat-app",
            false,
            Some(&workspace.join("Cargo.toml")),
        );

        assert!(build_info.features.contains(&"ax-feat/smp".to_string()));
    }

    #[test]
    fn max_cpu_num_does_not_duplicate_existing_smp_feature() {
        let mut build_info = ArceosBuildInfo {
            features: vec!["ax-std".to_string(), "ax-std/smp".to_string()],
            max_cpu_num: Some(4),
            ..ArceosBuildInfo::default()
        };

        build_info.resolve_features("ax-helloworld", false);

        assert_eq!(
            build_info
                .features
                .iter()
                .filter(|feature| feature.as_str() == "ax-std/smp")
                .count(),
            1
        );
    }

    #[test]
    fn resolve_build_info_path_uses_package_directory() {
        let path = resolve_build_info_path("ax-helloworld", "aarch64-unknown-none-softfloat", None)
            .unwrap();

        assert!(
            path.ends_with(
                "os/arceos/examples/helloworld/.build-aarch64-unknown-none-softfloat.toml"
            )
        );
    }

    #[test]
    fn resolve_build_info_path_prefers_explicit_path() {
        let path = resolve_build_info_path(
            "ax-helloworld",
            "aarch64-unknown-none-softfloat",
            Some(PathBuf::from("/tmp/custom-build.toml")),
        )
        .unwrap();

        assert_eq!(path, PathBuf::from("/tmp/custom-build.toml"));
    }

    #[test]
    fn resolve_build_info_path_in_dir_prefers_existing_bare_name() {
        let root = tempdir().unwrap();
        let bare = root
            .path()
            .join("build-aarch64-unknown-none-softfloat.toml");
        let dotted = root
            .path()
            .join(".build-aarch64-unknown-none-softfloat.toml");
        fs::write(&bare, "").unwrap();
        fs::write(&dotted, "").unwrap();

        let path = resolve_build_info_path_in_dir(root.path(), "aarch64-unknown-none-softfloat");

        assert_eq!(path, bare);
    }

    #[test]
    fn resolve_build_info_path_in_dir_falls_back_to_dotted_default() {
        let root = tempdir().unwrap();

        let path = resolve_build_info_path_in_dir(root.path(), "aarch64-unknown-none-softfloat");

        assert_eq!(
            path,
            root.path()
                .join(".build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn load_build_info_creates_missing_default_file() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        let request = request("ax-helloworld", "target", None, path.clone());

        let build_info = load_build_info(&request).unwrap();

        assert_eq!(build_info, ArceosBuildInfo::default_for_target("target"));
        assert!(path.exists());
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains("features = [\"ax-std\"]")
        );
    }

    #[test]
    fn load_build_info_creates_aarch64_default_with_dynamic_platform_enabled() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-aarch64.toml");
        let request = request(
            "ax-helloworld",
            "aarch64-unknown-none-softfloat",
            None,
            path,
        );

        let build_info = load_build_info(&request).unwrap();

        assert!(build_info.plat_dyn);
    }

    #[test]
    fn load_build_info_reads_existing_file() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
features = ["ax-std", "net"]
log = "Debug"
max_cpu_num = 4

[env]
AX_IP = "127.0.0.1"
"#,
        )
        .unwrap();
        let request = request("ax-helloworld", "target", None, path);

        let build_info = load_build_info(&request).unwrap();

        assert_eq!(build_info.log, LogLevel::Debug);
        assert_eq!(build_info.max_cpu_num, Some(4));
        assert!(build_info.features.contains(&"net".to_string()));
        assert_eq!(build_info.env.get("AX_IP"), Some(&"127.0.0.1".to_string()));
    }

    #[test]
    fn load_build_info_normalizes_legacy_feature_aliases() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        fs::write(
            &path,
            r#"
features = ["axstd", "axstd/smp", "axfeat/net"]
log = "Warn"

[env]
AX_IP = "10.0.2.15"
"#,
        )
        .unwrap();
        let request = request("ax-helloworld", "target", None, path.clone());

        let build_info = load_build_info(&request).unwrap();

        assert!(build_info.features.contains(&"ax-std".to_string()));
        assert!(build_info.features.contains(&"ax-std/smp".to_string()));
        assert!(build_info.features.contains(&"ax-feat/net".to_string()));
        assert!(!build_info.features.contains(&"axstd".to_string()));
        assert!(!build_info.features.contains(&"axstd/smp".to_string()));
        assert!(!build_info.features.contains(&"axfeat/net".to_string()));

        let rewritten = fs::read_to_string(path).unwrap();
        assert!(rewritten.contains("ax-std"));
        assert!(rewritten.contains("ax-std/smp"));
        assert!(rewritten.contains("ax-feat/net"));
        assert!(!rewritten.contains("axstd"));
        assert!(!rewritten.contains("axfeat/net"));
    }

    #[test]
    fn load_build_info_rejects_zero_max_cpu_num() {
        let err = ArceosBuildInfo {
            max_cpu_num: Some(0),
            ..ArceosBuildInfo::default()
        }
        .validated_max_cpu_num()
        .unwrap_err()
        .to_string();

        assert!(err.contains("max_cpu_num must be greater than 0"));
    }

    #[test]
    fn to_cargo_config_includes_ax_log_env() {
        let root = tempdir().unwrap();
        let request = request(
            "ax-helloworld",
            "aarch64-unknown-none-softfloat",
            None,
            root.path().join(".build.toml"),
        );

        let cargo = ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat")
            .into_cargo_config(&request)
            .unwrap();

        assert_eq!(cargo.env.get("AX_LOG"), Some(&"warn".to_string()));
    }

    #[test]
    fn to_cargo_config_maps_max_cpu_num_to_smp_env_for_dynamic_platforms() {
        let root = tempdir().unwrap();
        let request = request(
            "ax-helloworld",
            "aarch64-unknown-none-softfloat",
            Some(true),
            root.path().join(".build.toml"),
        );

        let cargo = ArceosBuildInfo {
            max_cpu_num: Some(4),
            ..ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat")
        }
        .into_cargo_config(&request)
        .unwrap();

        assert_eq!(cargo.env.get("SMP"), Some(&"4".to_string()));
        assert!(cargo.features.contains(&"ax-std/smp".to_string()));
    }

    #[test]
    fn to_cargo_config_maps_single_cpu_to_smp_env_without_forcing_smp_feature() {
        let root = tempdir().unwrap();
        let request = request(
            "ax-helloworld",
            "aarch64-unknown-none-softfloat",
            Some(true),
            root.path().join(".build.toml"),
        );

        let cargo = ArceosBuildInfo {
            max_cpu_num: Some(1),
            ..ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat")
        }
        .into_cargo_config(&request)
        .unwrap();

        assert_eq!(cargo.env.get("SMP"), Some(&"1".to_string()));
        assert!(!cargo.features.contains(&"ax-std/smp".to_string()));
    }

    #[test]
    fn base_cargo_config_defaults_to_bin_false_for_x86_64_targets() {
        let cargo = ArceosBuildInfo::default_for_target("x86_64-unknown-none")
            .into_base_cargo_config_with_log(
                "ax-helloworld".to_string(),
                "x86_64-unknown-none".to_string(),
                vec![],
            );

        assert!(!cargo.to_bin);
    }

    #[test]
    fn base_cargo_config_keeps_to_bin_true_for_non_x86_64_targets() {
        let cargo = ArceosBuildInfo::default_for_target("aarch64-unknown-none-softfloat")
            .into_base_cargo_config_with_log(
                "ax-helloworld".to_string(),
                "aarch64-unknown-none-softfloat".to_string(),
                vec![],
            );

        assert!(cargo.to_bin);
    }

    #[test]
    fn resolve_platform_package_prefers_matching_explicit_platform_dependency() {
        let platform = resolve_platform_package(
            "ax-helloworld-myplat",
            "aarch64-unknown-none-softfloat",
            &["aarch64-qemu-virt".to_string()],
        )
        .unwrap();

        assert_eq!(platform, "ax-plat-aarch64-qemu-virt");
    }

    #[test]
    fn find_local_platform_config_path_resolves_repo_platforms() {
        let path = find_local_platform_config_path("ax-plat-aarch64-qemu-virt")
            .unwrap()
            .expect("repo-local platform config should exist");

        assert!(path.ends_with(
            "components/axplat_crates/platforms/axplat-aarch64-qemu-virt/axconfig.toml"
        ));
    }

    #[test]
    fn find_local_platform_config_path_resolves_workspace_platform_dir() {
        let path = find_local_platform_config_path("axplat-riscv64-qemu-virt-hv")
            .unwrap()
            .expect("workspace platform config should exist");

        assert!(path.ends_with("platform/riscv64-qemu-virt/axconfig.toml"));
    }

    #[test]
    fn build_info_toml_equivalent_config_converts_to_non_dynamic_cargo() {
        let toml = r#"
features = ["ax-std"]
log = "Info"
plat_dyn = true
max_cpu_num = 4

[env]
AX_IP = "10.0.2.15"
AX_GW = "10.0.2.2"
"#;

        let build_info: ArceosBuildInfo =
            toml::from_str(toml).expect("build info should deserialize");
        let app_dir = resolve_package_manifest_path("ax-helloworld", None)
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let generated_config = app_dir.join(".axconfig.toml");
        let existed = generated_config.exists();
        let request = request(
            "ax-helloworld",
            "aarch64-unknown-none-softfloat",
            Some(false),
            app_dir.join(".build-aarch64-unknown-none-softfloat.toml"),
        );

        let cargo = build_info.into_cargo_config(&request).unwrap();

        assert!(cargo.features.contains(&"ax-std/defplat".to_string()));
        assert!(cargo.features.contains(&"ax-std/smp".to_string()));
        assert!(!cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(cargo.args.iter().any(|arg| arg.contains("-Tlinker.x")));
        assert_eq!(cargo.env.get("SMP"), Some(&"4".to_string()));
        assert_eq!(
            cargo.env.get("AX_CONFIG_PATH"),
            Some(&generated_config.display().to_string())
        );
        assert_eq!(
            cargo.env.get("AX_PLATFORM"),
            Some(&"aarch64-qemu-virt".to_string())
        );
        assert!(
            fs::read_to_string(&generated_config)
                .unwrap()
                .contains("max-cpu-num = 4")
        );

        if !existed && generated_config.exists() {
            fs::remove_file(generated_config).unwrap();
        }
    }

    #[test]
    fn parse_makefile_features_splits_commas_whitespace_and_dedups() {
        assert_eq!(
            parse_makefile_features(" lockdep, sched-rr  lockdep\taxfeat/net "),
            vec![
                "lockdep".to_string(),
                "sched-rr".to_string(),
                "axfeat/net".to_string()
            ]
        );
    }

    #[test]
    fn load_build_info_with_makefile_features_adds_axstd_prefixed_features() {
        let root = tempdir().unwrap();
        let path = root.path().join(".build-target.toml");
        let request = request("ax-helloworld", "target", None, path);

        let build_info =
            load_build_info_with_makefile_features(&request, &[String::from("lockdep")]).unwrap();

        assert!(build_info.features.contains(&"ax-std".to_string()));
        assert!(build_info.features.contains(&"ax-std/lockdep".to_string()));
    }

    #[test]
    fn apply_makefile_features_uses_axfeat_prefix_for_axfeat_packages() {
        let workspace = temp_workspace("ax-feat-app", "ax-feat = \"0.1.0\"\n").unwrap();
        let manifest_path = workspace.join("Cargo.toml");
        let mut build_info = ArceosBuildInfo {
            features: Vec::new(),
            ..ArceosBuildInfo::default()
        };

        apply_makefile_features_with_manifest_path(
            &mut build_info,
            "ax-feat-app",
            &[String::from("lockdep")],
            Some(&manifest_path),
        );

        assert!(build_info.features.contains(&"ax-feat/lockdep".to_string()));
        assert!(!build_info.features.contains(&"ax-std/lockdep".to_string()));
    }

    #[test]
    fn resolve_effective_plat_dyn_uses_override_and_target_support() {
        assert!(resolve_effective_plat_dyn(
            "aarch64-unknown-none-softfloat",
            true,
            None
        ));
        assert!(!resolve_effective_plat_dyn(
            "aarch64-unknown-none-softfloat",
            true,
            Some(false)
        ));
        assert!(resolve_effective_plat_dyn(
            "aarch64-unknown-none-softfloat",
            false,
            Some(true)
        ));
        assert!(!resolve_effective_plat_dyn(
            "x86_64-unknown-none",
            true,
            Some(true)
        ));
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
    fn resolve_platform_package_ignores_unselected_axplat_dependency() {
        let package = resolve_platform_package(
            "starryos",
            "riscv64gc-unknown-none-elf",
            &["qemu".to_string()],
        )
        .unwrap();

        assert_eq!(package, "ax-plat-riscv64-qemu-virt");
    }

    #[test]
    fn resolve_platform_package_prefers_x86_alias_myplat_dependency() {
        let package = resolve_platform_package(
            "axvisor",
            "x86_64-unknown-none",
            &["ax-std/myplat".to_string()],
        )
        .unwrap();

        assert_eq!(package, "axplat-x86-qemu-q35");
    }

    #[test]
    fn resolve_platform_package_prefers_riscv_myplat_dependency() {
        let package = resolve_platform_package(
            "axvisor",
            "riscv64gc-unknown-none-elf",
            &["ax-std/myplat".to_string()],
        )
        .unwrap();

        assert_eq!(package, "axplat-riscv64-qemu-virt-hv");
    }
}
