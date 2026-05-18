use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, anyhow, bail};
use ax_config_gen::{GenerateOptions, generate_config, read_config_string};
use cargo_metadata::{Metadata, Package};
use log::{info, warn};
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::context::{axbuild_tmp_dir, workspace_manifest_path, workspace_metadata_root_manifest};

fn env_truthy(env: &HashMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "1" | "true" | "on"
        )
    })
}

fn toolchain_rustflags(env: &HashMap<String, String>) -> Vec<String> {
    let mut flags = Vec::new();
    let dwarf = env_truthy(env, "DWARF");
    let backtrace = env_truthy(env, "BACKTRACE") || dwarf;

    if dwarf {
        flags.push("-Cdebuginfo=2".to_string());
        flags.push("-Cstrip=none".to_string());
    }

    if backtrace {
        flags.push("-Cforce-frame-pointers=yes".to_string());
    }

    flags
}

const LOONGARCH64_HERMIT_JSON: &str =
    include_str!("../../../os/arceos/examples/std/loongarch64-unknown-hermit.json");

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
    /// Additional config value overrides applied when generating `.axconfig.toml`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub axconfig_overrides: Vec<String>,
    /// Whether to use the dynamic platform linker flow when supported.
    #[serde(default, skip_serializing_if = "is_false")]
    pub plat_dyn: bool,
    /// Build this package as an ArceOS std/Hermit application.
    #[serde(default, rename = "std", skip_serializing_if = "is_false")]
    pub std_build: bool,
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

    pub(crate) fn into_prepared_base_cargo_config_with_metadata(
        mut self,
        package: &str,
        target: &str,
        plat_dyn_override: Option<bool>,
        metadata: &Metadata,
    ) -> anyhow::Result<Cargo> {
        if self.std_build {
            self.validated_max_cpu_num()?;
            let std_target = std_build_target_for(target)?;
            let mut cargo = self.into_base_cargo_config_with_log(
                package.to_string(),
                std_target.target,
                std_target.cargo_args,
            );
            cargo.env.extend(std_target.env);
            prepare_std_build_env(&mut cargo.env, target, metadata)?;
            cargo.extra_config = Some(std_cargo_config_path()?.display().to_string());
            cargo.to_bin = false;
            return Ok(cargo);
        }

        let plat_dyn = self.effective_plat_dyn(target, plat_dyn_override);
        self.validated_max_cpu_num()?;
        self.prepare_non_dynamic_platform_for(package, target, plat_dyn, metadata)?;
        self.resolve_features_with_metadata(package, plat_dyn, metadata);
        let mut extra_rustflags = toolchain_rustflags(&self.env);
        if self.features.iter().any(|f| f == "kcov") {
            extra_rustflags.push("-Cllvm-args=-sanitizer-coverage-level=3".to_string());
            extra_rustflags.push("-Cllvm-args=-sanitizer-coverage-trace-pc".to_string());
            extra_rustflags.push("-Cpasses=sancov-module".to_string());
        }
        let args = Self::build_cargo_args(target, plat_dyn, &extra_rustflags);

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

        let platform = resolve_platform_config(package, target, &self.features, metadata)?;
        let out_config = generated_axconfig_path(package, target)?;

        generate_axconfig(
            &crate::context::workspace_root_path()?,
            target,
            &platform.name,
            &platform.config_path,
            &out_config,
            self.validated_max_cpu_num()?,
            &self.axconfig_overrides,
        )?;

        self.env.insert(
            "AX_CONFIG_PATH".to_string(),
            out_config.display().to_string(),
        );
        self.env.insert("AX_PLATFORM".to_string(), platform.name);

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

    pub(crate) fn build_cargo_args(
        target: &str,
        plat_dyn: bool,
        extra_rustflags: &[String],
    ) -> Vec<String> {
        let mut args = Vec::new();
        args.push("--config".to_string());
        let mut rustflags: Vec<String> = Vec::new();
        rustflags.extend(extra_rustflags.iter().cloned());
        if plat_dyn {
            rustflags.push("-Clink-arg=-Taxplat.x".to_string());
        } else {
            rustflags.push("-Clink-arg=-Tlinker.x".to_string());
            rustflags.push("-Clink-arg=-no-pie".to_string());
            rustflags.push("-Clink-arg=-znostart-stop-gc".to_string());
        }

        let mut rendered = format!("target.{target}.rustflags=[");
        for (i, flag) in rustflags.iter().enumerate() {
            if i > 0 {
                rendered.push(',');
            }
            rendered.push_str(&format!("{flag:?}"));
        }
        rendered.push(']');
        args.push(rendered);
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
            std_build: false,
        }
    }
}

struct StdBuildTarget {
    target: String,
    cargo_args: Vec<String>,
    env: HashMap<String, String>,
}

fn std_build_target_for(target: &str) -> anyhow::Result<StdBuildTarget> {
    if target.starts_with("x86_64-") {
        Ok(StdBuildTarget {
            target: "x86_64-unknown-hermit".to_string(),
            cargo_args: Vec::new(),
            env: HashMap::new(),
        })
    } else if target.starts_with("aarch64-") {
        Ok(StdBuildTarget {
            target: "aarch64-unknown-hermit".to_string(),
            cargo_args: Vec::new(),
            env: HashMap::new(),
        })
    } else if target.starts_with("riscv64") {
        Ok(StdBuildTarget {
            target: "riscv64gc-unknown-hermit".to_string(),
            cargo_args: Vec::new(),
            env: HashMap::new(),
        })
    } else if target.starts_with("loongarch64-") {
        let path = std_loongarch64_target_json_path()?;
        Ok(StdBuildTarget {
            target: path.display().to_string(),
            cargo_args: vec!["-Z".to_string(), "json-target-spec".to_string()],
            env: [(
                "CARGO_UNSTABLE_JSON_TARGET_SPEC".to_string(),
                "true".to_string(),
            )]
            .into(),
        })
    } else {
        bail!("unsupported ArceOS std target triple `{target}`")
    }
}

fn prepare_std_build_env(
    envs: &mut HashMap<String, String>,
    target: &str,
    metadata: &Metadata,
) -> anyhow::Result<()> {
    let arch = target_arch_name(target)?;
    let platform_package = default_platform_package(arch);
    let platform_config = resolve_platform_config_by_package(platform_package, metadata)?;
    let out_config = generated_axconfig_path("arceos-rust", target)?;
    generate_axconfig(
        &crate::context::workspace_root_path()?,
        target,
        &platform_config.name,
        &platform_config.config_path,
        &out_config,
        envs.get("SMP")
            .map(|smp| {
                smp.parse()
                    .with_context(|| format!("invalid SMP value `{smp}`"))
            })
            .transpose()?,
        &[],
    )?;
    envs.insert(
        "ARCEOS_RUST_CONFIG".to_string(),
        out_config.display().to_string(),
    );
    Ok(())
}

fn std_cargo_config_path() -> anyhow::Result<PathBuf> {
    let path = std_build_dir()?.join("config.toml");
    write_if_changed(
        &path,
        r#"[unstable]
build-std = ["std", "panic_abort"]
build-std-features = []

[profile.release]
lto = false
panic = "abort"

[target.'cfg(target_os = "hermit")']
rustflags = [
    "-C", "link-arg=-no-pie",
    "-C", "link-arg=-Tlinker.x",
]
"#,
    )?;
    Ok(path)
}

fn std_loongarch64_target_json_path() -> anyhow::Result<PathBuf> {
    let path = std_build_dir()?.join("loongarch64-unknown-hermit.json");
    write_if_changed(&path, LOONGARCH64_HERMIT_JSON)?;
    Ok(path)
}

fn std_build_dir() -> anyhow::Result<PathBuf> {
    let dir = axbuild_tmp_dir(&crate::context::workspace_root_path()?).join("std");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create std build dir {}", dir.display()))?;
    Ok(dir)
}

fn write_if_changed(path: &Path, contents: &str) -> anyhow::Result<()> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create parent dir {}", parent.display()))?;
    }
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))
}

pub(crate) fn ensure_build_info<T>(path: &Path, default: impl FnOnce() -> T) -> anyhow::Result<()>
where
    T: Serialize,
{
    println!("Using build config: {}", path.display());

    if path.exists() {
        info!("Found build config at {}", path.display());
        return Ok(());
    }

    info!(
        "Build config not found at {}, writing default config",
        path.display()
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let default = default();
    std::fs::write(path, toml::to_string_pretty(&default)?)?;
    Ok(())
}

pub(crate) fn load_build_info<T>(path: &Path) -> anyhow::Result<T>
where
    T: DeserializeOwned,
{
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

pub(crate) fn cached_workspace_metadata() -> anyhow::Result<&'static Metadata> {
    static METADATA: OnceLock<anyhow::Result<Metadata, String>> = OnceLock::new();

    cached_metadata_result(
        METADATA.get_or_init(|| workspace_metadata().map_err(|err| format!("{err:#}"))),
    )
}

fn workspace_metadata_with_deps() -> anyhow::Result<Metadata> {
    let manifest_path = workspace_manifest_path()?;
    crate::context::workspace_metadata_root_manifest_with_deps(&manifest_path)
}

pub(crate) fn cached_workspace_metadata_with_deps() -> anyhow::Result<&'static Metadata> {
    static METADATA: OnceLock<anyhow::Result<Metadata, String>> = OnceLock::new();

    cached_metadata_result(
        METADATA.get_or_init(|| workspace_metadata_with_deps().map_err(|err| format!("{err:#}"))),
    )
}

fn cached_metadata_result(
    result: &'static anyhow::Result<Metadata, String>,
) -> anyhow::Result<&'static Metadata> {
    result.as_ref().map_err(|err| anyhow::anyhow!("{err}"))
}

fn workspace_package<'a>(metadata: &'a Metadata, package: &str) -> anyhow::Result<&'a Package> {
    metadata
        .packages
        .iter()
        .find(|pkg| metadata.workspace_members.contains(&pkg.id) && pkg.name == package)
        .ok_or_else(|| anyhow::anyhow!("workspace package `{package}` not found"))
}

fn metadata_package<'a>(metadata: &'a Metadata, package: &str) -> Option<&'a Package> {
    metadata.packages.iter().find(|pkg| pkg.name == package)
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
        "x86_64" => &[
            "axplat-x86-",
            "axplat-x86_64-",
            "ax-plat-x86-",
            "ax-plat-x86_64-",
        ],
        "aarch64" => &["axplat-aarch64-", "ax-plat-aarch64-"],
        "riscv64" => &["axplat-riscv64-", "ax-plat-riscv64-"],
        "loongarch64" => &["axplat-loongarch64-", "ax-plat-loongarch64-"],
        _ => &[],
    }
}

fn linker_platform_name(platform_package: &str) -> &str {
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

fn find_local_platform_config_path(
    platform_package: &str,
    metadata: &Metadata,
) -> anyhow::Result<Option<PathBuf>> {
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

fn read_platform_name(platform_config: &Path) -> Option<String> {
    read_config_string(&[platform_config.to_path_buf()], "platform").ok()
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

    fn metadata_for_manifest_with_deps(manifest_path: &Path) -> cargo_metadata::Metadata {
        crate::context::workspace_metadata_root_manifest_with_deps(manifest_path).unwrap()
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

    fn add_platform_package(
        workspace: &Path,
        package_name: &str,
        config_package_name: &str,
    ) -> anyhow::Result<()> {
        let platform_dir = workspace.join("platform");
        fs::create_dir_all(platform_dir.join("src"))?;
        fs::write(
            platform_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{package_name}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
            ),
        )?;
        fs::write(platform_dir.join("src/lib.rs"), "")?;
        fs::write(
            platform_dir.join("axconfig.toml"),
            format!(
                "arch = \"aarch64\" # str\nplatform = \"custom-board\" # str\npackage = \
                 \"{config_package_name}\" # str\n"
            ),
        )?;
        Ok(())
    }

    #[test]
    fn toolchain_rustflags_preserves_debug_and_backtrace_env() {
        let env = HashMap::from([("DWARF".to_string(), "1".to_string())]);

        assert_eq!(
            toolchain_rustflags(&env),
            vec![
                "-Cdebuginfo=2".to_string(),
                "-Cstrip=none".to_string(),
                "-Cforce-frame-pointers=yes".to_string(),
            ]
        );
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

    #[test]
    fn resolve_platform_config_path_uses_workspace_config() {
        let metadata = repo_metadata();
        let deps_metadata = workspace_metadata_with_deps().unwrap();
        let path =
            resolve_platform_config_path("axplat-riscv64-qemu-virt-hv", &metadata, &deps_metadata)
                .unwrap();

        assert!(path.ends_with("platform/riscv64-qemu-virt/axconfig.toml"));
    }

    #[test]
    fn resolve_platform_config_path_uses_dependency_config() {
        let workspace = temp_workspace(
            "custom-app",
            "ax-plat-aarch64-custom = { path = \"../platform\" }\n",
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
            resolve_platform_config_path("ax-plat-aarch64-custom", &metadata, &deps_metadata)
                .unwrap();

        assert!(path.ends_with("platform/axconfig.toml"));
    }
}
