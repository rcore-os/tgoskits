#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, anyhow, bail};
use ax_config_gen::{GenerateOptions, generate_config, read_config_string};
use cargo_metadata::{Metadata, Package};
use log::{info, warn};
use ostool::build::config::Cargo;
pub use ostool::build::config::LogLevel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

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

/// Whether the build config enables target backtrace support (frame pointers / unwind).
///
/// Matches [`toolchain_rustflags`]: `BACKTRACE=y` or `DWARF=y` in `[env]`.
pub(crate) fn build_info_enables_backtrace(info: &BuildInfo) -> bool {
    let dwarf = env_truthy(&info.env, "DWARF");
    env_truthy(&info.env, "BACKTRACE") || dwarf
}

/// Read a per-target `build-*.toml` and check [`build_info_enables_backtrace`].
pub(crate) fn build_info_enables_backtrace_path(path: &Path) -> bool {
    load_build_info::<BuildInfo>(path)
        .ok()
        .is_some_and(|info| build_info_enables_backtrace(&info))
}

const TARGET_JSON_ROOT: &str = "scripts/targets";
const NO_PIE_TARGET_DIR: &str = "no-pie";
const PIE_TARGET_DIR: &str = "pie";
pub(crate) const ARCEOS_LINKER_SCRIPT: &str = "linker.x";
const STD_TARGET_DIR: &str = "std";
const AXSTD_STD_PACKAGE: &str = "ax-std";

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
            plat_dyn: defaults_to_platform_dynamic(target),
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
        self.into_base_cargo_config_with_to_bin(
            package,
            target.clone(),
            args,
            default_to_bin_for_target(&target),
        )
    }

    fn into_base_cargo_config_with_to_bin(
        self,
        package: String,
        target: String,
        args: Vec<String>,
        to_bin: bool,
    ) -> Cargo {
        Cargo {
            env: self.env,
            target,
            package,
            features: self.features,
            log: Some(self.log),
            extra_config: None,
            profile: None,
            disable_someboot_build_config: true,
            args,
            pre_build_cmds: vec![],
            post_build_cmds: vec![],
            to_bin,
            bin: None,
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
        self.validated_max_cpu_num()?;
        let plat_dyn = self.effective_plat_dyn(target, plat_dyn_override);
        self.resolve_std_features_with_metadata(package, target, plat_dyn, metadata);
        let axconfig_overrides = self.axconfig_overrides.clone();
        let std_target = std_build_target_for(target, plat_dyn)?;
        let fake_lib_dir = std_fake_lib_dir(&std_target.target_name)?;
        let wrapper = std_linker_wrapper_path(&std_target.target_name, &fake_lib_dir, plat_dyn)?;
        let mut cargo = self.into_base_cargo_config_with_log(
            package.to_string(),
            std_target.target.clone(),
            std_target.cargo_args,
        );
        cargo.env.extend(std_target.env);
        prepare_std_build_env_for_package(
            &mut cargo.env,
            package,
            target,
            plat_dyn,
            &cargo.features,
            metadata,
            &axconfig_overrides,
        )?;
        let app_features = package_feature_names(package, metadata)?;
        let axstd_features = package_feature_names(AXSTD_STD_PACKAGE, metadata)?;
        inject_arceos_feature_for_std_build(&mut cargo.features, &app_features);
        pass_std_build_nested_features(
            &mut cargo.env,
            &mut cargo.features,
            &app_features,
            &axstd_features,
        );
        cargo.pre_build_cmds.push(
            std_fake_lib_prebuild_script_path(&std_target.target_name, &fake_lib_dir, &cargo.env)?
                .display()
                .to_string(),
        );
        cargo.extra_config = Some(
            std_cargo_config_path(&std_target.target_name, &wrapper, plat_dyn)?
                .display()
                .to_string(),
        );
        cargo.to_bin = default_to_bin_for_target_config(target, plat_dyn);
        Ok(cargo)
    }

    pub(crate) fn into_bare_cargo_config_with_metadata(
        mut self,
        package: &str,
        target: &str,
        plat_dyn_override: Option<bool>,
        metadata: &Metadata,
    ) -> anyhow::Result<Cargo> {
        let plat_dyn = self.effective_plat_dyn(target, plat_dyn_override);
        self.validated_max_cpu_num()?;
        self.prepare_non_dynamic_platform_for(package, target, plat_dyn, metadata)?;
        self.resolve_features_with_metadata(package, target, plat_dyn, metadata);
        let cargo_target = cargo_target_json_path(target, plat_dyn)?;
        let cargo_target = cargo_target.display().to_string();
        let rustflags = toolchain_rustflags(&self.env);
        let args = Self::build_cargo_args(&cargo_target, &rustflags);
        self.env.insert(
            "CARGO_UNSTABLE_JSON_TARGET_SPEC".to_string(),
            "true".to_string(),
        );

        self.prepare_log_env();
        self.prepare_max_cpu_num_env()
            .expect("max_cpu_num validation should run before cargo config generation");
        Ok(self.into_base_cargo_config_with_to_bin(
            package.to_string(),
            cargo_target,
            args,
            default_to_bin_for_target_config(target, plat_dyn),
        ))
    }

    fn resolve_std_features(&mut self) {
        self.features = self
            .features
            .iter()
            .map(|feature| normalize_std_feature(feature))
            .collect();
        self.features.sort();
        self.features.dedup();
    }

    fn resolve_std_features_with_metadata(
        &mut self,
        package: &str,
        target: &str,
        plat_dyn: bool,
        metadata: &Metadata,
    ) {
        self.features
            .extend(std_package_metadata_features(package, metadata));
        self.resolve_std_features();

        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push("smp".to_string());
        }
        if plat_dyn {
            self.features.push("smp".to_string());
            self.features.push("plat-dyn".to_string());
            self.features.push("ax-driver/plat-dyn".to_string());
        } else if !has_myplat_feature(&self.features)
            && !has_defplat_feature(&self.features)
            && !has_ax_hal_platform_feature(&self.features, Some(metadata))
        {
            self.features.push(
                default_ax_hal_platform_feature(target, Some(metadata))
                    .unwrap_or_else(|_| "ax-hal/defplat".to_string()),
            );
        }

        self.resolve_std_features();
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
        target: &str,
        plat_dyn: bool,
        metadata: &Metadata,
    ) {
        self.resolve_features_with_prefix_family(
            package,
            target,
            plat_dyn,
            detect_ax_feature_prefix_family(package, metadata),
            Some(metadata),
        );
    }

    fn resolve_features_with_prefix_family(
        &mut self,
        package: &str,
        target: &str,
        plat_dyn: bool,
        prefix_family: anyhow::Result<AxFeaturePrefixFamily>,
        metadata: Option<&Metadata>,
    ) {
        let prefix_family = self.resolve_ax_feature_prefix_family(package, prefix_family);
        let has_myplat = has_myplat_feature(&self.features);
        let has_defplat = has_defplat_feature(&self.features);

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
        } else if has_defplat {
            self.features
                .push(format!("{}defplat", prefix_family.prefix()));
        }

        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push(format!("{}smp", prefix_family.prefix()));
        }
        self.push_platform_feature(target, plat_dyn, has_myplat, metadata);

        self.features.sort();
        self.features.dedup();
    }

    fn push_platform_feature(
        &mut self,
        target: &str,
        plat_dyn: bool,
        has_myplat: bool,
        metadata: Option<&Metadata>,
    ) {
        if plat_dyn || has_myplat || has_ax_hal_platform_feature(&self.features, metadata) {
            return;
        }

        let feature = default_ax_hal_platform_feature(target, metadata)
            .unwrap_or_else(|_| "ax-hal/defplat".to_string());
        self.features.push(feature);
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
    pub(crate) fn resolve_features(&mut self, package: &str, target: &str, plat_dyn: bool) {
        match workspace_metadata() {
            Ok(metadata) => {
                self.resolve_features_with_metadata(package, target, plat_dyn, &metadata)
            }
            Err(err) => self.resolve_features_with_prefix_family(
                package,
                target,
                plat_dyn,
                Err(err.context("failed to load workspace metadata")),
                None,
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

    pub(crate) fn build_cargo_args(target: &str, extra_rustflags: &[String]) -> Vec<String> {
        let mut args = vec![
            "-Z".to_string(),
            "json-target-spec".to_string(),
            "-Z".to_string(),
            "build-std=core,alloc".to_string(),
        ];

        if !extra_rustflags.is_empty() {
            // Cargo resolves `target.<name>.rustflags` for a JSON target spec by the
            // spec file *stem* (e.g. `x86_64-unknown-none`), not the path passed to
            // `--target`. Using the full path as the key makes cargo silently drop the
            // entry, so flags like `-Cforce-frame-pointers=yes` never reach rustc.
            let target_key = Path::new(target)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or(target);
            args.push("--config".to_string());
            let rustflags_toml = toml::Value::Array(
                extra_rustflags
                    .iter()
                    .cloned()
                    .map(toml::Value::String)
                    .collect(),
            )
            .to_string();
            args.push(format!("target.{target_key}.rustflags={rustflags_toml}"));
        }
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

pub(crate) fn cargo_target_json_path(target: &str, plat_dyn: bool) -> anyhow::Result<PathBuf> {
    let target_file = match target {
        "aarch64-unknown-none-softfloat"
        | "riscv64gc-unknown-none-elf"
        | "x86_64-unknown-none"
        | "loongarch64-unknown-none-softfloat" => format!("{target}.json"),
        _ => bail!("unsupported target triple `{target}`"),
    };

    if plat_dyn {
        if !matches!(
            target,
            "aarch64-unknown-none-softfloat" | "riscv64gc-unknown-none-elf" | "x86_64-unknown-none"
        ) {
            bail!("unsupported PIE target `{target}`");
        }
        Ok(Path::new(TARGET_JSON_ROOT)
            .join(PIE_TARGET_DIR)
            .join(target_file))
    } else {
        Ok(Path::new(TARGET_JSON_ROOT)
            .join(NO_PIE_TARGET_DIR)
            .join(target_file))
    }
}

struct StdBuildTarget {
    target_name: String,
    target: String,
    cargo_args: Vec<String>,
    env: HashMap<String, String>,
}

fn std_build_target_for(target: &str, plat_dyn: bool) -> anyhow::Result<StdBuildTarget> {
    let (target_name, tool_prefix) = if target.starts_with("x86_64-") {
        ("x86_64-unknown-linux-musl", "x86_64-linux-musl")
    } else if target.starts_with("aarch64-") {
        ("aarch64-unknown-linux-musl", "aarch64-linux-musl")
    } else if target.starts_with("riscv64") {
        ("riscv64gc-unknown-linux-musl", "riscv64-linux-musl")
    } else if target.starts_with("loongarch64-") {
        ("loongarch64-unknown-linux-musl", "loongarch64-linux-musl")
    } else {
        bail!("unsupported ArceOS std target triple `{target}`");
    };

    let mut env = HashMap::new();
    env.insert(
        "CARGO_UNSTABLE_JSON_TARGET_SPEC".to_string(),
        "true".to_string(),
    );
    env.extend(std_c_toolchain_env(target_name, tool_prefix));

    Ok(StdBuildTarget {
        target_name: target_name.to_string(),
        target: std_target_json_path(target_name, plat_dyn)
            .display()
            .to_string(),
        cargo_args: vec!["-Z".to_string(), "json-target-spec".to_string()],
        env,
    })
}

fn std_c_toolchain_env(target_name: &str, tool_prefix: &str) -> HashMap<String, String> {
    let mut env = HashMap::new();
    let target_env = target_name.replace('-', "_");
    let cc = format!("{tool_prefix}-cc");
    let ar = format!("{tool_prefix}-ar");
    let c_flags = std_c_target_flags(target_name).join(" ");
    env.insert(format!("CC_{target_env}"), cc.clone());
    env.insert(format!("AR_{target_env}"), ar);
    if !c_flags.is_empty() {
        env.insert(format!("CFLAGS_{target_env}"), c_flags.clone());
        env.insert(format!("CXXFLAGS_{target_env}"), c_flags.clone());
    }

    if let Some(sysroot) = musl_toolchain_sysroot(&cc) {
        let mut bindgen_args = vec![
            format!("--target={tool_prefix}"),
            format!("--sysroot={sysroot}"),
        ];
        bindgen_args.extend(
            std_c_target_flags(target_name)
                .into_iter()
                .map(str::to_string),
        );
        env.insert(
            format!("BINDGEN_EXTRA_CLANG_ARGS_{target_env}"),
            bindgen_args.join(" "),
        );
    }

    env
}

fn std_c_target_flags(target_name: &str) -> Vec<&'static str> {
    if target_name.starts_with("x86_64-") {
        vec![
            "-mno-mmx",
            "-mno-sse",
            "-mno-sse2",
            "-mno-sse3",
            "-mno-ssse3",
            "-mno-sse4.1",
            "-mno-sse4.2",
            "-mno-avx",
            "-mno-avx2",
            "-msoft-float",
        ]
    } else if target_name.starts_with("aarch64-") {
        vec!["-mgeneral-regs-only"]
    } else if target_name.starts_with("riscv64") {
        vec!["-march=rv64gc", "-mabi=lp64d", "-mcmodel=medany"]
    } else if target_name.starts_with("loongarch64-") {
        vec!["-mabi=lp64s", "-msoft-float"]
    } else {
        Vec::new()
    }
}

fn musl_toolchain_sysroot(cc: &str) -> Option<String> {
    let output = std::process::Command::new(cc)
        .arg("-print-sysroot")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sysroot = String::from_utf8(output.stdout).ok()?;
    let sysroot = sysroot.trim();
    (!sysroot.is_empty()).then(|| sysroot.to_string())
}

fn std_target_json_path(target: &str, plat_dyn: bool) -> PathBuf {
    let path = Path::new(TARGET_JSON_ROOT).join(STD_TARGET_DIR);
    if plat_dyn {
        path.join(PIE_TARGET_DIR).join(format!("{target}.json"))
    } else {
        path.join(format!("{target}.json"))
    }
}

pub(crate) fn prepare_std_build_env(
    envs: &mut HashMap<String, String>,
    target: &str,
    plat_dyn: bool,
    metadata: &Metadata,
) -> anyhow::Result<()> {
    prepare_std_build_env_for_package(
        envs,
        AXSTD_STD_PACKAGE,
        target,
        plat_dyn,
        &[],
        metadata,
        &[],
    )
}

fn prepare_std_build_env_for_package(
    envs: &mut HashMap<String, String>,
    package: &str,
    target: &str,
    plat_dyn: bool,
    features: &[String],
    metadata: &Metadata,
    axconfig_overrides: &[String],
) -> anyhow::Result<()> {
    envs.insert("AX_TARGET".to_string(), target.to_string());

    if plat_dyn {
        return Ok(());
    }

    let platform_config = resolve_platform_config(package, target, features, metadata)?;
    let out_config = generated_axconfig_path(package, target)?;
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
        axconfig_overrides,
    )?;
    envs.insert(
        "AX_CONFIG_PATH".to_string(),
        out_config.display().to_string(),
    );
    envs.insert("AX_PLATFORM".to_string(), platform_config.name);
    Ok(())
}

fn pass_std_build_nested_features(
    _envs: &mut HashMap<String, String>,
    features: &mut Vec<String>,
    app_features: &[String],
    axstd_features: &[String],
) {
    let mut cargo_features = Vec::new();

    for feature in features.drain(..) {
        let feature = normalize_std_feature(&feature);
        if matches!(feature.as_str(), "ax-std" | "ax-feat") {
            continue;
        }
        if is_log_level_feature(&feature) {
            continue;
        }
        if is_axstd_std_check_feature(&feature) {
            if std_feature_stays_on_app(&feature, app_features) {
                cargo_features.push(feature.clone());
            }
            let axstd_feature = axstd_feature_name(&feature);
            if axstd_feature_is_available(axstd_feature, axstd_features) {
                cargo_features.push(format!("ax-std/{axstd_feature}"));
            } else if feature.contains('/') {
                cargo_features.push(feature);
            }
        } else {
            cargo_features.push(feature);
        }
    }

    cargo_features.sort();
    cargo_features.dedup();

    *features = cargo_features;
}

fn inject_arceos_feature_for_std_build(features: &mut Vec<String>, app_features: &[String]) {
    if app_features.iter().any(|feature| feature == "arceos")
        && !features.iter().any(|feature| feature == "arceos")
    {
        features.push("arceos".to_string());
    }
}

fn axstd_feature_name(feature: &str) -> &str {
    feature
        .strip_prefix("ax-hal/")
        .or_else(|| feature.strip_prefix("ax-driver/"))
        .unwrap_or(feature)
}

fn package_feature_names(package: &str, metadata: &Metadata) -> anyhow::Result<Vec<String>> {
    Ok(workspace_package(metadata, package)?
        .features
        .keys()
        .cloned()
        .collect())
}

fn axstd_feature_is_available(feature: &str, axstd_features: &[String]) -> bool {
    axstd_features
        .iter()
        .any(|axstd_feature| axstd_feature == feature)
}

fn std_cargo_config_path(target: &str, linker: &Path, plat_dyn: bool) -> anyhow::Result<PathBuf> {
    let mode = std_link_mode_suffix(plat_dyn);
    let path = std_build_dir()?.join(format!("config-{target}-{mode}.toml"));
    let rustflags = std_rustflags_toml(target, plat_dyn);
    write_if_changed(
        &path,
        &format!(
            r#"[unstable]
build-std = ["std", "panic_abort"]
build-std-features = []

[profile.release]
lto = false
panic = "abort"

[target.{target}]
linker = "{}"
rustflags = [
{}
]
"#,
            toml_escape_string(&linker.display().to_string()),
            rustflags,
        ),
    )?;
    Ok(path)
}

fn std_link_mode_suffix(plat_dyn: bool) -> &'static str {
    if plat_dyn { "dynamic" } else { "static" }
}

fn std_rustflags_toml(target: &str, plat_dyn: bool) -> String {
    let flags = [
        r#"    "--cfg", "arceos_std","#,
        r#"    "--check-cfg=cfg(arceos_std)","#,
    ];
    let _ = (target, plat_dyn);
    flags.join("\n")
}

fn std_fake_lib_dir(target: &str) -> anyhow::Result<PathBuf> {
    let dir = axbuild_tmp_dir(&crate::context::workspace_root_path()?)
        .join("std-libs")
        .join(target)
        .join("release");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create std fake lib dir {}", dir.display()))?;
    Ok(dir)
}

fn std_linker_wrapper_path(
    target: &str,
    fake_lib_dir: &Path,
    plat_dyn: bool,
) -> anyhow::Result<PathBuf> {
    let mode = std_link_mode_suffix(plat_dyn);
    let path = std_build_dir()?.join(format!("linker-{target}-{mode}.sh"));
    write_if_changed(
        &path,
        &std_linker_wrapper_script(target, fake_lib_dir, plat_dyn)?,
    )?;
    set_executable(&path)?;
    Ok(path)
}

fn std_fake_lib_prebuild_script_path(
    target_name: &str,
    fake_lib_dir: &Path,
    envs: &HashMap<String, String>,
) -> anyhow::Result<PathBuf> {
    let contents = std_fake_lib_prebuild_script(target_name, fake_lib_dir, envs);
    let hash = short_content_hash(&contents);
    let path = std_build_dir()?
        .join("prebuild")
        .join(format!("prebuild-{target_name}-{hash}.sh"));
    write_if_changed(&path, &contents)?;
    set_executable(&path)?;
    Ok(path)
}

fn std_fake_lib_prebuild_script(
    target_name: &str,
    fake_lib_dir: &Path,
    envs: &HashMap<String, String>,
) -> String {
    let mut env_exports = Vec::new();
    let mut sorted_envs: Vec<_> = envs.iter().collect();
    sorted_envs.sort_by_key(|(key, _)| *key);
    for (key, value) in sorted_envs {
        if key
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_uppercase() || ch.is_ascii_digit())
        {
            env_exports.push(format!("export {key}={}", shell_single_quote(value)));
        }
    }

    format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

{}

target_name={}
fake_dir={}

archive_tool() {{
    if command -v rust-ar >/dev/null 2>&1; then
        command -v rust-ar
        return
    fi

    local sysroot_llvm_ar
    sysroot_llvm_ar="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin/llvm-ar"
    if [[ -x "$sysroot_llvm_ar" ]]; then
        printf '%s\n' "$sysroot_llvm_ar"
        return
    fi

    if command -v llvm-ar >/dev/null 2>&1; then
        command -v llvm-ar
        return
    fi

    if command -v ar >/dev/null 2>&1; then
        command -v ar
        return
    fi

    echo "failed to find archive tool; tried rust-ar, rust toolchain llvm-ar, llvm-ar, ar" >&2
    exit 127
}}

create_empty_archive() {{
    local archive="$1"
    rm -f "$archive"
    "$(archive_tool)" crs "$archive"
}}

mkdir -p "$fake_dir"
create_empty_archive "$fake_dir/libc.a"
create_empty_archive "$fake_dir/libunwind.a"
"#,
        env_exports.join("\n"),
        shell_single_quote(target_name),
        shell_single_quote(&fake_lib_dir.display().to_string()),
    )
}

fn std_linker_wrapper_script(
    target: &str,
    fake_lib_dir: &Path,
    plat_dyn: bool,
) -> anyhow::Result<String> {
    let machine = lld_machine_for_std_target(target)?;
    let dynamic_platform = if plat_dyn { "1" } else { "0" };
    Ok(format!(
        r#"#!/usr/bin/env bash
set -euo pipefail

fake_dir={}
target_name={}
lld_args=("-m" "{}")
link_search_dirs=()
archive_args=()
dynamic_platform={}

add_link_search_dir() {{
    local dir="$1"
    [[ -n "$dir" ]] && link_search_dirs+=("$dir")
}}

append_lld_arg() {{
    local arg="$1"
    case "$arg" in
        *.a|*.rlib)
            archive_args+=("$arg")
            ;;
        *)
            flush_archive_group
            lld_args+=("$arg")
            ;;
    esac
}}

flush_archive_group() {{
    if (( ${{#archive_args[@]}} == 0 )); then
        return
    fi
    lld_args+=("--start-group" "${{archive_args[@]}}" "--end-group")
    archive_args=()
}}

find_linker_script() {{
    local dir
    for dir in "${{link_search_dirs[@]}}"; do
        if [[ -f "$dir/linker.x" ]]; then
            printf '%s\n' "$dir/linker.x"
            return
        fi
    done
}}

add_arg() {{
    local arg="$1"
    if [[ "${{expect_link_search_dir:-0}}" == "1" ]]; then
        add_link_search_dir "$arg"
        append_lld_arg "$arg"
        expect_link_search_dir=0
        return
    fi

    if [[ "${{skip_next_lld_driver_arg:-0}}" == "1" ]]; then
        skip_next_lld_driver_arg=0
        return
    fi

    case "$arg" in
        */rcrt1.o|rcrt1.o|*/crt1.o|crt1.o|*/Scrt1.o|Scrt1.o|*/crti.o|crti.o|*/crtn.o|crtn.o|*/crtbegin*.o|crtbegin*.o|*/crtend*.o|crtend*.o)
            return
            ;;
        -L)
            append_lld_arg "$arg"
            expect_link_search_dir=1
            return
            ;;
        -L*)
            add_link_search_dir "${{arg#-L}}"
            append_lld_arg "$arg"
            return
            ;;
        -flavor)
            skip_next_lld_driver_arg=1
            return
            ;;
        -flavor=*|-T*)
            return
            ;;
        -lc)
            append_lld_arg "$fake_dir/libc.a"
            return
            ;;
        -lgcc_s|-lgcc)
            return
            ;;
        -lunwind)
            if [[ -f "$fake_dir/libunwind.a" ]]; then
                append_lld_arg "$fake_dir/libunwind.a"
            fi
            return
            ;;
        -static-pie)
            if [[ "$dynamic_platform" == "1" ]]; then
                append_lld_arg "-pie"
            else
                append_lld_arg "-static"
                append_lld_arg "-no-pie"
            fi
            return
            ;;
        -static)
            if [[ "$dynamic_platform" == "0" ]]; then
                append_lld_arg "-static"
            fi
            return
            ;;
        -pie)
            if [[ "$dynamic_platform" == "1" ]]; then
                append_lld_arg "-pie"
            fi
            return
            ;;
        -no-pie)
            if [[ "$dynamic_platform" == "0" ]]; then
                append_lld_arg "-no-pie"
            fi
            return
            ;;
        -nostartfiles|-nodefaultlibs|-nostdlib|-m*)
            return
            ;;
        --eh-frame-hdr|-z|relro|norelro|now|noexecstack)
            return
            ;;
        -Wl,*)
            local IFS=,
            local parts=(${{arg#-Wl,}})
            local part
            for part in "${{parts[@]}}"; do
                [[ -n "$part" ]] && add_arg "$part"
            done
            return
            ;;
    esac
    append_lld_arg "$arg"
}}

for arg in "$@"; do
    add_arg "$arg"
done

linker_script="$(find_linker_script)"
if [[ -z "$linker_script" ]]; then
    echo "failed to find linker.x in current linker search dirs for $target_name" >&2
    exit 1
fi

flush_archive_group
lld_args+=("-L$fake_dir" "-T$linker_script")
exec rust-lld -flavor gnu "${{lld_args[@]}}"
"#,
        shell_single_quote(&fake_lib_dir.display().to_string()),
        shell_single_quote(target),
        machine,
        dynamic_platform,
    ))
}

fn lld_machine_for_std_target(target: &str) -> anyhow::Result<&'static str> {
    if target.starts_with("x86_64-") {
        Ok("elf_x86_64")
    } else if target.starts_with("aarch64-") {
        Ok("aarch64elf")
    } else if target.starts_with("riscv64") {
        Ok("elf64lriscv")
    } else if target.starts_with("loongarch64-") {
        Ok("elf64loongarch")
    } else {
        bail!("unsupported ArceOS std linker target `{target}`")
    }
}

fn toml_escape_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn short_content_hash(contents: &str) -> String {
    let digest = Sha256::digest(contents.as_bytes());
    format!("{digest:x}").chars().take(16).collect()
}

fn set_executable(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
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
    let tmp_path = temporary_sibling_path(path);
    fs::write(&tmp_path, contents)
        .with_context(|| format!("failed to write {}", tmp_path.display()))?;
    if let Err(err) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(path);
        fs::rename(&tmp_path, path).with_context(|| {
            format!(
                "failed to replace {} with {} after initial rename error: {err}",
                path.display(),
                tmp_path.display()
            )
        })?;
    }
    Ok(())
}

fn temporary_sibling_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "axbuild".into());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(".{file_name}.{}.{}.tmp", std::process::id(), nanos))
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
    let contents = std::fs::read_to_string(path)?;
    reject_removed_std_field(path, &contents)?;
    toml::from_str::<T>(&contents)
        .with_context(|| format!("failed to parse build info {}", path.display()))
}

pub(crate) fn apply_target_defaults_if_plat_dyn_unspecified(
    build_info: &mut BuildInfo,
    target: &str,
    content: &str,
) {
    if build_info_declares_plat_dyn(content) {
        return;
    }

    if target.starts_with("aarch64-") || target.starts_with("riscv64") {
        build_info.plat_dyn = BuildInfo::default_for_target(target).plat_dyn;
    }
}

fn build_info_declares_plat_dyn(content: &str) -> bool {
    toml::from_str::<toml::Value>(content)
        .ok()
        .and_then(|value| value.as_table().cloned())
        .is_some_and(|table| table.contains_key("plat_dyn") || table.contains_key("plat-dyn"))
}

pub(crate) fn reject_removed_std_field(path: &Path, contents: &str) -> anyhow::Result<()> {
    if let Ok(table) = toml::from_str::<toml::Table>(contents)
        && table.contains_key("std")
    {
        bail!(
            "build config {} uses removed `std` field; std-aware Rust builds are now the default, \
             remove `std = ...`",
            path.display()
        );
    }

    Ok(())
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
    target.starts_with("aarch64-") || target.starts_with("riscv64") || target.starts_with("x86_64-")
}

fn defaults_to_platform_dynamic(target: &str) -> bool {
    target.starts_with("aarch64-") || target.starts_with("riscv64") || target.starts_with("x86_64-")
}

fn default_to_bin_for_target(target: &str) -> bool {
    !target.starts_with("x86_64-") && !target.starts_with("loongarch64-")
}

fn default_to_bin_for_target_config(target: &str, plat_dyn: bool) -> bool {
    default_to_bin_for_target(target) || (plat_dyn && target.starts_with("x86_64-"))
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

fn normalize_std_feature(feature: &str) -> String {
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

fn is_axstd_std_check_feature(feature: &str) -> bool {
    matches!(feature, "ax-std" | "ax-feat")
        || feature.starts_with("ax-hal/")
        || feature.starts_with("ax-driver/")
        || is_known_axstd_feature(feature)
}

fn std_feature_stays_on_app(feature: &str, app_features: &[String]) -> bool {
    if feature == "arceos" {
        return true;
    }
    !is_axstd_std_check_feature(feature)
        || app_features
            .iter()
            .any(|app_feature| app_feature == feature)
}

fn is_known_axstd_feature(feature: &str) -> bool {
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

fn is_log_level_feature(feature: &str) -> bool {
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
fn apply_makefile_features_with_prefix_family(
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

fn apply_std_makefile_features(build_info: &mut BuildInfo, makefile_features: &[String]) {
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

fn has_myplat_feature(features: &[String]) -> bool {
    features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "myplat" | "ax-std/myplat" | "ax-feat/myplat" | "ax-hal/myplat"
        )
    })
}

fn has_defplat_feature(features: &[String]) -> bool {
    features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "defplat" | "ax-std/defplat" | "ax-feat/defplat" | "ax-hal/defplat"
        )
    })
}

fn ax_hal_platform_feature_name<'a>(
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

fn is_known_ax_hal_platform_feature(platform: &str) -> bool {
    matches!(
        platform,
        "x86-pc"
            | "riscv64-sg2002"
            | "riscv64-visionfive2"
            | "loongarch64-qemu-virt"
            | "x86-qemu-q35"
    )
}

fn has_ax_hal_platform_feature(features: &[String], metadata: Option<&Metadata>) -> bool {
    features
        .iter()
        .any(|feature| ax_hal_platform_feature_name(feature, metadata).is_some())
}

fn default_ax_hal_platform_feature(
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
        "x86_64" => "ax-hal/x86-pc",
        "loongarch64" => "ax-hal/loongarch64-qemu-virt",
        "aarch64" | "riscv64" => {
            return Err(anyhow!(
                "no static default ax-hal platform for arch `{arch}`"
            ));
        }
        _ => unreachable!("unsupported arch"),
    }
    .to_string())
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
struct AxplatMetadata {
    platform: String,
    arch: String,
    config: Option<PathBuf>,
    default_for_arch: bool,
    dynamic: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
#[serde(rename_all = "kebab-case")]
struct AxstdMetadata {
    features: Vec<String>,
}

#[derive(Debug, Clone)]
struct PlatformPackage {
    package: String,
    manifest_dir: PathBuf,
    metadata: AxplatMetadata,
}

fn platform_metadata(package: &Package) -> Option<AxplatMetadata> {
    package
        .metadata
        .get("axplat")
        .cloned()
        .and_then(|metadata| serde_json::from_value(metadata).ok())
}

fn axstd_metadata(package: &Package) -> Option<AxstdMetadata> {
    package
        .metadata
        .get("axstd")
        .cloned()
        .and_then(|metadata| serde_json::from_value(metadata).ok())
}

fn std_package_metadata_features(package: &str, metadata: &Metadata) -> Vec<String> {
    metadata_package(metadata, package)
        .and_then(axstd_metadata)
        .map(|metadata| metadata.features)
        .unwrap_or_default()
}

fn platform_packages(metadata: &Metadata) -> Vec<PlatformPackage> {
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

fn platform_package_by_name(metadata: &Metadata, platform_name: &str) -> Option<String> {
    platform_packages(metadata)
        .into_iter()
        .find(|platform| platform.metadata.platform == platform_name)
        .map(|platform| platform.package)
}

fn platform_package_by_name_with_workspace_fallback(
    metadata: &Metadata,
    platform_name: &str,
) -> Option<String> {
    platform_package_by_name(metadata, platform_name).or_else(|| {
        cached_workspace_metadata()
            .ok()
            .and_then(|metadata| platform_package_by_name(metadata, platform_name))
    })
}

fn default_platform_package(metadata: &Metadata, arch: &str) -> Option<String> {
    platform_packages(metadata)
        .into_iter()
        .find(|platform| {
            platform.metadata.arch == arch
                && platform.metadata.default_for_arch
                && !platform.metadata.dynamic
        })
        .map(|platform| platform.package)
}

fn default_platform_package_with_workspace_fallback(
    metadata: &Metadata,
    arch: &str,
) -> Option<String> {
    default_platform_package(metadata, arch).or_else(|| {
        cached_workspace_metadata()
            .ok()
            .and_then(|metadata| default_platform_package(metadata, arch))
    })
}

fn platform_config_path_from_metadata(
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

fn ax_hal_platform_package(platform: &str, metadata: &Metadata) -> Option<String> {
    platform_package_by_name_with_workspace_fallback(metadata, platform)
}

fn require_default_platform_package(metadata: &Metadata, arch: &str) -> anyhow::Result<String> {
    default_platform_package_with_workspace_fallback(metadata, arch)
        .ok_or_else(|| anyhow!("no default platform package is registered for arch `{arch}`"))
}

fn resolve_platform_package(
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

fn explicit_platform_package_from_features(
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

fn dependency_is_platform(dep_name: &str) -> bool {
    dep_name.starts_with("axplat-") || dep_name.starts_with("ax-plat-")
}

fn feature_enables_dependency(package_info: &Package, feature: &str, dep_name: &str) -> bool {
    package_info.features.get(feature).is_some_and(|items| {
        items
            .iter()
            .any(|item| item == dep_name || item == &format!("dep:{dep_name}"))
    })
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

fn read_platform_name(platform_config: &Path) -> Option<String> {
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

    fn gnu_lld_pre_link_args(spec: &serde_json::Value) -> Vec<&str> {
        spec["pre-link-args"]["gnu-lld"]
            .as_array()
            .unwrap()
            .iter()
            .map(|arg| arg.as_str().unwrap())
            .collect()
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
        let platform_dir = workspace.join("platforms");
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
    fn build_info_enables_backtrace_matches_env_flags() {
        let mut info = BuildInfo::default();
        assert!(!build_info_enables_backtrace(&info));

        info.env.insert("BACKTRACE".to_string(), "y".to_string());
        assert!(build_info_enables_backtrace(&info));

        info.env.clear();
        info.env.insert("DWARF".to_string(), "1".to_string());
        assert!(build_info_enables_backtrace(&info));
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
    fn std_build_nested_features_are_passed_through_not_enabled_on_app() {
        let mut envs = HashMap::new();
        let mut features = vec![
            "ax-hal/loongarch64-qemu-virt".to_string(),
            "ax-driver/plat-dyn".to_string(),
            "ax-driver/virtio-blk".to_string(),
            "ax-driver/virtio-net".to_string(),
            "dns".to_string(),
        ];

        pass_std_build_nested_features(
            &mut envs,
            &mut features,
            &["dns".to_string()],
            &[
                "dns".to_string(),
                "loongarch64-qemu-virt".to_string(),
                "plat-dyn".to_string(),
                "virtio-blk".to_string(),
                "virtio-net".to_string(),
            ],
        );

        assert_eq!(
            features,
            vec![
                "ax-std/dns".to_string(),
                "ax-std/loongarch64-qemu-virt".to_string(),
                "ax-std/plat-dyn".to_string(),
                "ax-std/virtio-blk".to_string(),
                "ax-std/virtio-net".to_string(),
                "dns".to_string(),
            ]
        );
        assert!(envs.is_empty());
    }

    #[test]
    fn std_build_runtime_features_are_passed_through_after_normalization() {
        let mut info = BuildInfo {
            features: vec![
                "ax-hal/loongarch64-qemu-virt".to_string(),
                "dns".to_string(),
            ],
            ..BuildInfo::default()
        };

        info.resolve_std_features();
        let mut envs = HashMap::new();
        pass_std_build_nested_features(
            &mut envs,
            &mut info.features,
            &["dns".to_string()],
            &["dns".to_string(), "loongarch64-qemu-virt".to_string()],
        );

        assert_eq!(
            info.features,
            vec![
                "ax-std/dns".to_string(),
                "ax-std/loongarch64-qemu-virt".to_string(),
                "dns".to_string()
            ]
        );
        assert!(envs.is_empty());
    }

    #[test]
    fn std_build_cargo_config_builds_fake_lib_before_app() {
        let metadata = repo_metadata();
        let cargo = BuildInfo {
            features: vec![
                "ax-std".to_string(),
                "ax-hal/x86-pc".to_string(),
                "fs".to_string(),
                "dns".to_string(),
            ],
            ..BuildInfo::default()
        }
        .into_prepared_base_cargo_config_with_metadata(
            "arceos-std-helloworld",
            "x86_64-unknown-none",
            None,
            &metadata,
        )
        .unwrap();

        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/x86_64-unknown-linux-musl.json")
        );
        assert!(
            cargo
                .args
                .windows(2)
                .any(|pair| pair == ["-Z", "json-target-spec"])
        );
        assert_eq!(
            cargo.features,
            vec![
                "arceos".to_string(),
                "ax-std/dns".to_string(),
                "ax-std/fs".to_string(),
                "ax-std/x86-pc".to_string(),
            ]
        );
        assert!(!cargo.to_bin);
        assert_eq!(
            cargo.env.get("CARGO_UNSTABLE_JSON_TARGET_SPEC"),
            Some(&"true".to_string())
        );
        assert!(!cargo.env.contains_key("AXSTD_STD_DEFAULT_FEATURES"));
        assert_eq!(
            cargo.env.get("AX_TARGET"),
            Some(&"x86_64-unknown-none".to_string())
        );
        assert!(
            cargo
                .extra_config
                .as_ref()
                .is_some_and(|path| path.ends_with("config-x86_64-unknown-linux-musl-static.toml"))
        );
        assert_eq!(cargo.pre_build_cmds.len(), 1);
        let prebuild = fs::read_to_string(&cargo.pre_build_cmds[0]).unwrap();
        assert!(prebuild.contains("target_name='x86_64-unknown-linux-musl'"));
        assert!(!prebuild.contains("cargo}\" build -p ax-std"));
        assert!(!prebuild.contains("libax_std.a"));
        assert!(prebuild.contains("libc.a"));
        assert!(prebuild.contains("archive_tool()"));
        assert!(prebuild.contains("$(rustc --print sysroot)"));
        assert!(prebuild.contains("create_empty_archive \"$fake_dir/libc.a\""));
        assert!(prebuild.contains("create_empty_archive \"$fake_dir/libunwind.a\""));
    }

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

[env]
AX_IP = "10.0.2.15"
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
    fn std_build_uses_package_axstd_metadata_for_ax_std_features() {
        let workspace = temp_workspace("std-app", "").unwrap();
        let app_manifest = workspace.join("app/Cargo.toml");
        fs::write(
            &app_manifest,
            "[package]\nname = \"std-app\"\nversion = \"0.1.0\"\nedition = \
             \"2024\"\n\n[package.metadata.axstd]\nfeatures = [\"multitask\", \"net\", \
             \"log-level-debug\"]\n",
        )
        .unwrap();

        let metadata = metadata_for_manifest(&workspace.join("Cargo.toml"));
        let mut info = BuildInfo {
            features: vec!["dns".to_string()],
            ..BuildInfo::default()
        };

        info.resolve_std_features_with_metadata("std-app", "x86_64-unknown-none", false, &metadata);
        let mut envs = HashMap::new();
        pass_std_build_nested_features(
            &mut envs,
            &mut info.features,
            &[],
            &[
                "dns".to_string(),
                "multitask".to_string(),
                "net".to_string(),
                "x86-pc".to_string(),
            ],
        );

        assert_eq!(
            info.features,
            vec![
                "ax-std/dns".to_string(),
                "ax-std/multitask".to_string(),
                "ax-std/net".to_string(),
                "ax-std/x86-pc".to_string(),
            ]
        );
        assert!(envs.is_empty());
    }

    #[test]
    fn std_build_auto_enables_app_arceos_feature_when_declared() {
        let metadata = repo_metadata();
        let cargo = BuildInfo {
            features: Vec::new(),
            ..BuildInfo::default()
        }
        .into_prepared_base_cargo_config_with_metadata(
            "arceos-std-helloworld",
            "x86_64-unknown-none",
            None,
            &metadata,
        )
        .unwrap();

        assert!(cargo.features.contains(&"arceos".to_string()));
    }

    #[test]
    fn std_build_does_not_inject_arceos_feature_when_app_lacks_it() {
        let mut features = vec!["dns".to_string()];

        inject_arceos_feature_for_std_build(&mut features, &["dns".to_string()]);

        assert_eq!(features, vec!["dns".to_string()]);
    }

    #[test]
    fn std_build_plat_dyn_uses_dynamic_platform_features_without_static_hal_platform() {
        let metadata = repo_metadata();
        let cargo = BuildInfo {
            plat_dyn: true,
            features: vec![
                "ax-std".to_string(),
                "ax-driver/virtio-net".to_string(),
                "net".to_string(),
            ],
            ..BuildInfo::default()
        }
        .into_prepared_base_cargo_config_with_metadata(
            "test-arceos-std-httpclient",
            "aarch64-unknown-none-softfloat",
            None,
            &metadata,
        )
        .unwrap();

        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
        );
        assert!(cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(cargo.features.contains(&"ax-std/smp".to_string()));
        assert!(cargo.features.contains(&"ax-std/virtio-net".to_string()));
        assert!(cargo.features.contains(&"ax-std/net".to_string()));
        assert!(cargo.to_bin);
        assert_eq!(
            cargo.env.get("AX_TARGET"),
            Some(&"aarch64-unknown-none-softfloat".to_string())
        );
        assert!(
            !cargo
                .features
                .contains(&"ax-std/aarch64-qemu-virt".to_string())
        );
    }

    #[test]
    fn std_build_aarch64_defaults_to_dynamic_platform() {
        let metadata = repo_metadata();
        let cargo = BuildInfo {
            ..BuildInfo::default_for_target("aarch64-unknown-none-softfloat")
        }
        .into_prepared_base_cargo_config_with_metadata(
            "test-arceos-std-helloworld",
            "aarch64-unknown-none-softfloat",
            None,
            &metadata,
        )
        .unwrap();

        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/aarch64-unknown-linux-musl.json")
        );
        assert!(!cargo.env.contains_key("AX_CONFIG_PATH"));
        assert!(cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(cargo.features.contains(&"ax-std/smp".to_string()));
        assert!(
            !cargo
                .features
                .contains(&"ax-std/aarch64-qemu-virt".to_string())
        );
        let config = std::fs::read_to_string(cargo.extra_config.unwrap()).unwrap();
        assert!(config.contains("arceos_std"));
        assert!(!config.contains("relocation-model"));
        assert!(!config.contains("code-model"));
    }

    #[test]
    fn std_build_target_maps_arceos_targets_to_linux_musl_by_link_mode() {
        let cases = [
            (
                "x86_64-unknown-none",
                false,
                "scripts/targets/std/x86_64-unknown-linux-musl.json",
            ),
            (
                "aarch64-unknown-none-softfloat",
                false,
                "scripts/targets/std/aarch64-unknown-linux-musl.json",
            ),
            (
                "aarch64-unknown-none-softfloat",
                true,
                "scripts/targets/std/pie/aarch64-unknown-linux-musl.json",
            ),
            (
                "riscv64gc-unknown-none-elf",
                false,
                "scripts/targets/std/riscv64gc-unknown-linux-musl.json",
            ),
            (
                "riscv64gc-unknown-none-elf",
                true,
                "scripts/targets/std/pie/riscv64gc-unknown-linux-musl.json",
            ),
            (
                "loongarch64-unknown-none-softfloat",
                false,
                "scripts/targets/std/loongarch64-unknown-linux-musl.json",
            ),
        ];

        for (bare_target, plat_dyn, expected_path) in cases {
            let mapped = std_build_target_for(bare_target, plat_dyn).unwrap();
            assert!(mapped.target.ends_with(expected_path));
            assert!(
                mapped
                    .cargo_args
                    .windows(2)
                    .any(|pair| pair == ["-Z", "json-target-spec"])
            );
            assert_eq!(
                mapped.env.get("CARGO_UNSTABLE_JSON_TARGET_SPEC"),
                Some(&"true".to_string())
            );
        }

        let riscv = std_build_target_for("riscv64gc-unknown-none-elf", true).unwrap();
        assert_eq!(
            riscv.env.get("CC_riscv64gc_unknown_linux_musl"),
            Some(&"riscv64-linux-musl-cc".to_string())
        );
        assert_eq!(
            riscv.env.get("AR_riscv64gc_unknown_linux_musl"),
            Some(&"riscv64-linux-musl-ar".to_string())
        );
        if let Some(bindgen_args) = riscv
            .env
            .get("BINDGEN_EXTRA_CLANG_ARGS_riscv64gc_unknown_linux_musl")
        {
            assert!(bindgen_args.contains("--target=riscv64-linux-musl"));
            assert!(bindgen_args.contains("--sysroot="));
            assert!(bindgen_args.contains("-march=rv64gc"));
            assert!(bindgen_args.contains("-mabi=lp64d"));
        }
    }

    #[test]
    fn std_c_toolchain_env_does_not_require_installed_cross_compiler() {
        let env = std_c_toolchain_env("riscv64gc-unknown-linux-musl", "definitely-missing-musl");

        assert_eq!(
            env.get("CC_riscv64gc_unknown_linux_musl"),
            Some(&"definitely-missing-musl-cc".to_string())
        );
        assert_eq!(
            env.get("AR_riscv64gc_unknown_linux_musl"),
            Some(&"definitely-missing-musl-ar".to_string())
        );
        assert_eq!(
            env.get("CFLAGS_riscv64gc_unknown_linux_musl"),
            Some(&"-march=rv64gc -mabi=lp64d -mcmodel=medany".to_string())
        );
        assert_eq!(
            env.get("CXXFLAGS_riscv64gc_unknown_linux_musl"),
            Some(&"-march=rv64gc -mabi=lp64d -mcmodel=medany".to_string())
        );
        assert!(!env.contains_key("BINDGEN_EXTRA_CLANG_ARGS_riscv64gc_unknown_linux_musl"));
    }

    #[test]
    fn std_c_toolchain_env_exports_loongarch_softfloat_abi_flags() {
        let env = std_c_toolchain_env("loongarch64-unknown-linux-musl", "loongarch64-linux-musl");

        assert_eq!(
            env.get("CFLAGS_loongarch64_unknown_linux_musl"),
            Some(&"-mabi=lp64s -msoft-float".to_string())
        );
        assert_eq!(
            env.get("CXXFLAGS_loongarch64_unknown_linux_musl"),
            Some(&"-mabi=lp64s -msoft-float".to_string())
        );
        if let Some(bindgen_args) =
            env.get("BINDGEN_EXTRA_CLANG_ARGS_loongarch64_unknown_linux_musl")
        {
            assert!(bindgen_args.contains("--target=loongarch64-linux-musl"));
            assert!(bindgen_args.contains("-mabi=lp64s"));
            assert!(bindgen_args.contains("-msoft-float"));
        }
    }

    #[test]
    fn std_target_specs_keep_none_kernel_fields_with_std_link_policy() {
        for (bare_target, std_target, plat_dyn) in [
            ("x86_64-unknown-none", "x86_64-unknown-linux-musl", false),
            (
                "aarch64-unknown-none-softfloat",
                "aarch64-unknown-linux-musl",
                false,
            ),
            (
                "aarch64-unknown-none-softfloat",
                "aarch64-unknown-linux-musl",
                true,
            ),
            (
                "riscv64gc-unknown-none-elf",
                "riscv64gc-unknown-linux-musl",
                false,
            ),
            (
                "riscv64gc-unknown-none-elf",
                "riscv64gc-unknown-linux-musl",
                true,
            ),
            (
                "loongarch64-unknown-none-softfloat",
                "loongarch64-unknown-linux-musl",
                false,
            ),
        ] {
            let workspace = crate::context::workspace_root_path().unwrap();
            let base_path = workspace.join(cargo_target_json_path(bare_target, plat_dyn).unwrap());
            let std_path = workspace.join(std_target_json_path(std_target, plat_dyn));
            assert!(
                std_path.exists(),
                "missing std target spec {}",
                std_path.display()
            );

            let base_spec: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&base_path).unwrap()).unwrap();
            let mut std_spec: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&std_path).unwrap()).unwrap();

            assert_eq!(std_spec["os"], "linux");
            assert_eq!(std_spec["env"], "musl");
            assert_eq!(std_spec["target-family"], serde_json::json!(["unix"]));
            assert_eq!(std_spec["has-thread-local"], true);
            let expected_tls_model = if std_target.starts_with("riscv64") {
                "local-exec"
            } else {
                "initial-exec"
            };
            assert_eq!(std_spec["tls-model"], expected_tls_model);
            assert_eq!(std_spec["metadata"]["std"], true);
            assert!(
                std_spec
                    .pointer("/metadata/description")
                    .and_then(|value| value.as_str())
                    .is_some_and(|description| description.contains("musl identity"))
            );
            assert_eq!(std_spec["llvm-target"], base_spec["llvm-target"]);
            assert_eq!(std_spec["eh-frame-header"], false);
            assert_eq!(std_spec["relro-level"], "off");

            std_spec.as_object_mut().unwrap().remove("os");
            std_spec.as_object_mut().unwrap().remove("env");
            std_spec.as_object_mut().unwrap().remove("target-family");
            std_spec.as_object_mut().unwrap().remove("has-thread-local");
            std_spec.as_object_mut().unwrap().remove("tls-model");
            std_spec["metadata"]["description"] = base_spec["metadata"]["description"].clone();
            std_spec["metadata"]["std"] = base_spec["metadata"]["std"].clone();
            std_spec["pre-link-args"] = base_spec["pre-link-args"].clone();
            {
                let std_spec = std_spec.as_object_mut().unwrap();
                if let Some(eh_frame_header) = base_spec.get("eh-frame-header") {
                    std_spec.insert("eh-frame-header".to_string(), eh_frame_header.clone());
                } else {
                    std_spec.remove("eh-frame-header");
                }
                if let Some(relro_level) = base_spec.get("relro-level") {
                    std_spec.insert("relro-level".to_string(), relro_level.clone());
                } else {
                    std_spec.remove("relro-level");
                }
            }

            assert_eq!(
                std_spec, base_spec,
                "std target {std_target} must keep all kernel fields from {bare_target}"
            );
        }

        let loongarch = serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(crate::context::workspace_root_path().unwrap().join(
                std_target_json_path("loongarch64-unknown-linux-musl", false),
            ))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(loongarch["llvm-abiname"], "lp64s");
        assert_eq!(loongarch["features"], "-f,-d");
    }

    #[test]
    fn std_target_specs_do_not_import_linux_userspace_link_fields() {
        for (target, plat_dyn) in [
            ("x86_64-unknown-linux-musl", false),
            ("aarch64-unknown-linux-musl", false),
            ("aarch64-unknown-linux-musl", true),
            ("riscv64gc-unknown-linux-musl", false),
            ("riscv64gc-unknown-linux-musl", true),
            ("loongarch64-unknown-linux-musl", false),
        ] {
            let path = crate::context::workspace_root_path()
                .unwrap()
                .join(std_target_json_path(target, plat_dyn));
            assert!(path.exists(), "missing std target spec {}", path.display());

            let spec: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            assert!(spec.get("dynamic-linking").is_none());
            assert!(spec.get("has-rpath").is_none());
            assert!(spec.get("pre-link-objects-fallback").is_none());
            assert!(spec.get("post-link-objects-fallback").is_none());
            assert!(spec.get("crt-static-default").is_none());
            assert!(spec.get("crt-static-respected").is_none());
            assert!(spec.get("supported-split-debuginfo").is_none());
            assert!(spec.get("supports-xray").is_none());
        }
    }

    #[test]
    fn std_target_specs_embed_final_link_policy() {
        let cases = [
            ("x86_64-unknown-linux-musl", false, "_start", "-no-pie"),
            ("aarch64-unknown-linux-musl", false, "_start", "-no-pie"),
            ("aarch64-unknown-linux-musl", true, "_head", "-pie"),
            ("riscv64gc-unknown-linux-musl", false, "_start", "-no-pie"),
            ("riscv64gc-unknown-linux-musl", true, "_head", "-pie"),
            ("loongarch64-unknown-linux-musl", false, "_start", "-no-pie"),
        ];

        for (target, plat_dyn, entry, mode_arg) in cases {
            let path = crate::context::workspace_root_path()
                .unwrap()
                .join(std_target_json_path(target, plat_dyn));
            let spec: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let link_args = gnu_lld_pre_link_args(&spec);

            assert!(link_args.contains(&mode_arg));
            assert!(link_args.contains(&"--gc-sections"));
            assert!(link_args.contains(&"-znorelro"));
            assert!(link_args.contains(&"-znostart-stop-gc"));
            assert!(link_args.contains(&"-Tlinker.x"));
            assert!(link_args.contains(&"-u"));
            assert!(link_args.contains(&entry));
            assert_eq!(spec["eh-frame-header"], false);
            assert_eq!(spec["relro-level"], "off");

            if plat_dyn {
                assert!(!link_args.contains(&"-static"));
                assert!(!link_args.contains(&"-no-pie"));
            } else {
                assert!(link_args.contains(&"-static"));
                assert!(!link_args.contains(&"-pie"));
            }
        }
    }

    #[test]
    fn std_cargo_config_uses_linux_musl_wrapper_and_arceos_std_cfg() {
        let fake_dir = std_fake_lib_dir("x86_64-unknown-linux-musl").unwrap();
        let wrapper =
            std_linker_wrapper_path("x86_64-unknown-linux-musl", &fake_dir, false).unwrap();
        let config = std_cargo_config_path("x86_64-unknown-linux-musl", &wrapper, false).unwrap();
        let config = fs::read_to_string(config).unwrap();

        assert!(config.contains("build-std = [\"std\", \"panic_abort\"]"));
        assert!(config.contains("build-std-features = []"));
        assert!(config.contains("[target.x86_64-unknown-linux-musl]"));
        assert!(config.contains("arceos_std"));
        assert!(config.contains("--check-cfg=cfg(arceos_std)"));
        assert!(config.contains(&wrapper.display().to_string()));
        assert!(!config.contains("relocation-model"));
        assert!(!config.contains("code-model"));
        assert!(!config.contains("target_os = \"hermit\""));
    }

    #[test]
    fn std_cargo_config_leaves_kernel_codegen_to_target_spec() {
        let fake_dir = std_fake_lib_dir("loongarch64-unknown-linux-musl").unwrap();
        let wrapper =
            std_linker_wrapper_path("loongarch64-unknown-linux-musl", &fake_dir, false).unwrap();
        let config =
            std_cargo_config_path("loongarch64-unknown-linux-musl", &wrapper, false).unwrap();
        let config = fs::read_to_string(config).unwrap();

        assert!(config.contains("arceos_std"));
        assert!(!config.contains("relocation-model"));
        assert!(!config.contains("code-model"));
    }

    #[test]
    fn std_cargo_config_uses_static_link_mode_without_codegen_override() {
        let fake_dir = std_fake_lib_dir("aarch64-unknown-linux-musl").unwrap();
        let wrapper =
            std_linker_wrapper_path("aarch64-unknown-linux-musl", &fake_dir, false).unwrap();
        let app_config =
            std_cargo_config_path("aarch64-unknown-linux-musl", &wrapper, false).unwrap();

        let config = fs::read_to_string(app_config).unwrap();
        assert!(config.contains("arceos_std"));
        assert!(!config.contains("relocation-model"));
        assert!(!config.contains("code-model"));
    }

    #[test]
    fn std_cargo_config_uses_dynamic_link_mode_without_codegen_override() {
        let fake_dir = std_fake_lib_dir("aarch64-unknown-linux-musl").unwrap();
        let wrapper =
            std_linker_wrapper_path("aarch64-unknown-linux-musl", &fake_dir, true).unwrap();
        let app_config =
            std_cargo_config_path("aarch64-unknown-linux-musl", &wrapper, true).unwrap();

        let config = fs::read_to_string(app_config).unwrap();
        assert!(config.contains("arceos_std"));
        assert!(!config.contains("relocation-model"));
        assert!(!config.contains("code-model"));
    }

    #[test]
    fn std_cargo_config_and_wrapper_paths_are_partitioned_by_link_mode() {
        let fake_dir = std_fake_lib_dir("aarch64-unknown-linux-musl").unwrap();
        let static_wrapper =
            std_linker_wrapper_path("aarch64-unknown-linux-musl", &fake_dir, false).unwrap();
        let dynamic_wrapper =
            std_linker_wrapper_path("aarch64-unknown-linux-musl", &fake_dir, true).unwrap();
        let static_config =
            std_cargo_config_path("aarch64-unknown-linux-musl", &static_wrapper, false).unwrap();
        let dynamic_config =
            std_cargo_config_path("aarch64-unknown-linux-musl", &dynamic_wrapper, true).unwrap();

        assert_ne!(static_wrapper, dynamic_wrapper);
        assert_ne!(static_config, dynamic_config);
        assert!(
            static_wrapper
                .display()
                .to_string()
                .ends_with("linker-aarch64-unknown-linux-musl-static.sh")
        );
        assert!(
            dynamic_wrapper
                .display()
                .to_string()
                .ends_with("linker-aarch64-unknown-linux-musl-dynamic.sh")
        );
        assert!(
            static_config
                .display()
                .to_string()
                .ends_with("config-aarch64-unknown-linux-musl-static.toml")
        );
        assert!(
            dynamic_config
                .display()
                .to_string()
                .ends_with("config-aarch64-unknown-linux-musl-dynamic.toml")
        );
    }

    #[test]
    fn std_linker_wrapper_filters_crt_and_replaces_fixed_libs() {
        let fake_dir = std_fake_lib_dir("x86_64-unknown-linux-musl").unwrap();
        let wrapper =
            std_linker_wrapper_path("x86_64-unknown-linux-musl", &fake_dir, false).unwrap();
        let wrapper = fs::read_to_string(wrapper).unwrap();

        assert!(wrapper.contains("rust-lld"));
        assert!(wrapper.contains("link_search_dirs=()"));
        assert!(wrapper.contains("archive_args=()"));
        assert!(wrapper.contains("add_link_search_dir"));
        assert!(wrapper.contains("append_lld_arg"));
        assert!(wrapper.contains("flush_archive_group"));
        assert!(wrapper.contains("--start-group"));
        assert!(wrapper.contains("--end-group"));
        assert!(wrapper.contains("find_linker_script"));
        assert!(wrapper.contains("failed to find linker.x in current linker search dirs"));
        assert!(!wrapper.contains("entry_symbol="));
        assert!(!wrapper.contains("link_mode_args="));
        assert!(wrapper.contains("dynamic_platform=0"));
        assert!(wrapper.contains("crtbegin"));
        assert!(wrapper.contains("static-pie"));
        assert!(wrapper.contains("-flavor"));
        assert!(wrapper.contains("-T*"));
        assert!(wrapper.contains("--eh-frame-hdr"));
        assert!(wrapper.contains("relro"));
        assert!(wrapper.contains("noexecstack"));
        assert!(!wrapper.contains("-znorelro"));
        assert!(!wrapper.contains("--gc-sections"));
        assert!(!wrapper.contains("-znostart-stop-gc"));
        assert!(wrapper.contains("libc.a"));
        assert!(wrapper.contains("libunwind.a"));
        assert!(wrapper.contains("-lgcc_s|-lgcc"));
        assert!(!wrapper.contains("--whole-archive"));
        assert!(!wrapper.contains("\"-u\""));
        assert!(!wrapper.contains("_start"));
    }

    #[test]
    fn std_linker_wrapper_uses_explicit_dynamic_platform_mode() {
        let fake_dir = std_fake_lib_dir("aarch64-unknown-linux-musl").unwrap();
        let wrapper =
            std_linker_wrapper_path("aarch64-unknown-linux-musl", &fake_dir, true).unwrap();
        let wrapper = fs::read_to_string(wrapper).unwrap();

        assert!(wrapper.contains("find_linker_script"));
        assert!(!wrapper.contains("latest_build_output_script axplat.x"));
        assert!(!wrapper.contains("entry_symbol="));
        assert!(!wrapper.contains("link_mode_args="));
        assert!(wrapper.contains("dynamic_platform=1"));
        assert!(!wrapper.contains("_head"));
    }

    #[test]
    fn std_build_dynamic_x86_64_prepares_binary_artifact() {
        let metadata = repo_metadata();
        let cargo = BuildInfo {
            ..BuildInfo::default_for_target("x86_64-unknown-none")
        }
        .into_prepared_base_cargo_config_with_metadata(
            "test-arceos-std-helloworld",
            "x86_64-unknown-none",
            None,
            &metadata,
        )
        .unwrap();

        assert!(
            cargo
                .target
                .ends_with("scripts/targets/std/pie/x86_64-unknown-linux-musl.json")
        );
        assert!(cargo.to_bin);
        assert!(cargo.features.contains(&"ax-std/plat-dyn".to_string()));
        assert!(cargo.features.contains(&"ax-std/smp".to_string()));
        assert_eq!(
            cargo.env.get("AX_TARGET"),
            Some(&"x86_64-unknown-none".to_string())
        );
    }

    #[test]
    fn std_build_plat_dyn_stays_on_arceos_rust_dependency() {
        let mut info = BuildInfo {
            features: vec!["ax-feat/plat-dyn".to_string(), "alloc".to_string()],
            ..BuildInfo::default()
        };

        info.resolve_std_features();
        let mut envs = HashMap::new();
        pass_std_build_nested_features(&mut envs, &mut info.features, &[], &["alloc".to_string()]);

        assert_eq!(info.features, vec!["ax-std/alloc".to_string()]);
        assert!(envs.is_empty());
    }

    #[test]
    fn cargo_target_json_path_maps_no_pie_targets() {
        let cases = [
            "aarch64-unknown-none-softfloat",
            "riscv64gc-unknown-none-elf",
            "x86_64-unknown-none",
            "loongarch64-unknown-none-softfloat",
        ];

        for target in cases {
            let path = cargo_target_json_path(target, false).unwrap();
            assert!(path.ends_with(format!("scripts/targets/no-pie/{target}.json")));
        }
    }

    #[test]
    fn cargo_target_json_path_maps_aarch64_pie_target() {
        let path = cargo_target_json_path("aarch64-unknown-none-softfloat", true).unwrap();

        assert!(path.ends_with("scripts/targets/pie/aarch64-unknown-none-softfloat.json"));
    }

    #[test]
    fn cargo_target_json_path_maps_riscv64_pie_target() {
        let path = cargo_target_json_path("riscv64gc-unknown-none-elf", true).unwrap();

        assert!(path.ends_with("scripts/targets/pie/riscv64gc-unknown-none-elf.json"));
    }

    #[test]
    fn cargo_target_json_path_maps_x86_64_dyn_to_pie_target() {
        let path = cargo_target_json_path("x86_64-unknown-none", true).unwrap();

        assert!(path.ends_with("scripts/targets/pie/x86_64-unknown-none.json"));
    }

    #[test]
    fn x86_64_defaults_to_dynamic_platform() {
        assert!(supports_platform_dynamic("x86_64-unknown-none"));
        assert!(BuildInfo::default_for_target("x86_64-unknown-none").plat_dyn);
        assert!(resolve_effective_plat_dyn(
            "x86_64-unknown-none",
            true,
            None
        ));
        assert!(default_to_bin_for_target_config(
            "x86_64-unknown-none",
            true
        ));
    }

    #[test]
    fn build_cargo_args_uses_json_target_spec_and_build_std() {
        let args = BuildInfo::build_cargo_args(
            "scripts/targets/no-pie/aarch64-unknown-none-softfloat.json",
            &[],
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

    #[test]
    fn build_cargo_args_uses_target_stem_as_rustflags_key() {
        let args = BuildInfo::build_cargo_args(
            "scripts/targets/no-pie/aarch64-unknown-none-softfloat.json",
            &["-Cforce-frame-pointers=yes".to_string()],
        );

        // Cargo matches the JSON target by file stem, so the config key must be the
        // stem (`aarch64-unknown-none-softfloat`) and not the full spec path.
        assert!(args.windows(2).any(|pair| {
            pair[0] == "--config"
                && pair[1].starts_with("target.aarch64-unknown-none-softfloat.rustflags=")
                && pair[1].contains("\"-Cforce-frame-pointers=yes\"")
        }));
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("scripts/targets/no-pie")),
            "config key must not use the spec path"
        );
    }

    #[test]
    fn target_specs_embed_only_final_linker_script() {
        let specs = [
            include_str!("../../targets/no-pie/aarch64-unknown-none-softfloat.json"),
            include_str!("../../targets/no-pie/loongarch64-unknown-none-softfloat.json"),
            include_str!("../../targets/no-pie/riscv64gc-unknown-none-elf.json"),
            include_str!("../../targets/no-pie/x86_64-unknown-none.json"),
            include_str!("../../targets/pie/aarch64-unknown-none-softfloat.json"),
            include_str!("../../targets/pie/riscv64gc-unknown-none-elf.json"),
            include_str!("../../targets/pie/x86_64-unknown-none.json"),
        ];

        for spec in specs {
            assert!(spec.contains("-Tlinker.x"));
            assert!(!spec.contains("-Taxplat.x"));
            assert!(!spec.contains("-Truntime.x"));
        }
    }

    #[test]
    fn x86_64_no_pie_target_preserves_pic_codegen_without_pie_link() {
        let spec: serde_json::Value = serde_json::from_str(include_str!(
            "../../targets/no-pie/x86_64-unknown-none.json"
        ))
        .unwrap();

        assert_eq!(spec["relocation-model"], "pic");
        assert_eq!(spec.get("position-independent-executables"), None);
        assert_eq!(spec.get("static-position-independent-executables"), None);

        let link_args = spec["pre-link-args"]["gnu-lld"].as_array().unwrap();
        assert!(link_args.iter().any(|arg| arg == "-no-pie"));
        assert!(!link_args.iter().any(|arg| arg == "-pie"));
    }

    #[test]
    fn x86_64_pie_target_uses_final_linker_script() {
        let spec: serde_json::Value =
            serde_json::from_str(include_str!("../../targets/pie/x86_64-unknown-none.json"))
                .unwrap();

        assert_eq!(spec["position-independent-executables"], true);
        assert_eq!(spec["static-position-independent-executables"], true);
        assert_eq!(spec["relocation-model"], "pic");

        let link_args = spec["pre-link-args"]["gnu-lld"].as_array().unwrap();
        assert!(link_args.iter().any(|arg| arg == "-pie"));
        assert!(link_args.iter().any(|arg| arg == "-znostart-stop-gc"));
        assert!(link_args.iter().any(|arg| arg == "-Tlinker.x"));
        assert!(!link_args.iter().any(|arg| arg == "-Taxplat.x"));
        assert!(!link_args.iter().any(|arg| arg == "-no-pie"));
    }

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
            &["lockdep".to_string(), "smp".to_string()],
        );

        assert_eq!(
            info.features,
            vec!["ax-std/lockdep".to_string(), "ax-std/smp".to_string()]
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
            "test-arceos-std-app",
            &[String::from("lockdep")],
            Err(anyhow::anyhow!("std test packages do not depend on ax-std")),
        );

        info.resolve_std_features();
        let mut envs = HashMap::new();
        pass_std_build_nested_features(
            &mut envs,
            &mut info.features,
            &[],
            &["lockdep".to_string()],
        );

        assert_eq!(info.features, vec!["ax-std/lockdep".to_string()]);
        assert!(envs.is_empty());
    }

    #[test]
    fn retired_static_aarch64_platform_features_are_not_ax_hal_platforms() {
        let metadata = repo_metadata();

        for feature in [
            "ax-hal/aarch64-qemu-virt",
            "ax-hal/aarch64-raspi",
            "ax-hal/aarch64-bsta1000b",
            "ax-hal/aarch64-phytium-pi",
        ] {
            assert_eq!(ax_hal_platform_feature_name(feature, Some(&metadata)), None);
        }
    }

    #[test]
    fn default_aarch64_platform_feature_falls_back_to_defplat() {
        let mut info = BuildInfo::default();

        info.resolve_features_with_prefix_family(
            "ax-helloworld",
            "aarch64-unknown-none-softfloat",
            false,
            Ok(AxFeaturePrefixFamily::AxStd),
            None,
        );

        assert!(info.features.contains(&"ax-hal/defplat".to_string()));
        assert!(
            !info
                .features
                .contains(&"ax-hal/aarch64-qemu-virt".to_string())
        );
    }

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
            resolve_platform_config_path("ax-plat-aarch64-custom", &metadata, &deps_metadata)
                .unwrap();

        assert!(path.ends_with("platforms/axconfig.toml"));
    }
}
