use super::*;

pub(crate) fn env_truthy(env: &HashMap<String, String>, key: &str) -> bool {
    env.get(key).is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "1" | "true" | "on"
        )
    })
}

pub(crate) fn toolchain_rustflags(env: &HashMap<String, String>) -> Vec<String> {
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

pub(super) fn features_enable_stack_protector(features: &[String]) -> bool {
    features.iter().any(|feature| {
        matches!(
            feature.as_str(),
            "stack-protector"
                | "ax-std/stack-protector"
                | "ax-feat/stack-protector"
                | "starry-kernel/stack-protector"
        )
    })
}

pub(crate) fn toolchain_rustflags_for_features(
    env: &HashMap<String, String>,
    features: &[String],
) -> Vec<String> {
    let mut flags = toolchain_rustflags(env);
    if features_enable_stack_protector(features) {
        flags.push("-Zstack-protector=strong".to_string());
    }
    flags
}

pub(crate) fn append_encoded_rustflags(cargo: &mut Cargo, flags: &[&str]) {
    const KEY: &str = "CARGO_ENCODED_RUSTFLAGS";
    let value = cargo.env.entry(KEY.to_string()).or_default();
    if !value.is_empty() {
        value.push('\x1f');
    }
    value.push_str(&flags.join("\x1f"));
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

pub(super) const TARGET_JSON_ROOT: &str = "scripts/targets";
pub(super) const PIE_TARGET_DIR: &str = "pie";
pub(crate) const ARCEOS_LINKER_SCRIPT: &str = "linker.x";
pub(super) const STD_TARGET_DIR: &str = "std";
pub(super) const AXSTD_STD_PACKAGE: &str = "ax-std";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AxFeaturePrefixFamily {
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
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
    #[serde(
        default = "default_plat_dyn",
        skip_serializing_if = "is_default_plat_dyn"
    )]
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

    pub(crate) fn into_base_cargo_config_with_to_bin(
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
        let rustflags = toolchain_rustflags_for_features(&cargo.env, &cargo.features);
        cargo.extra_config = Some(
            std_cargo_config_path(&std_target.target_name, &wrapper, plat_dyn, &rustflags)?
                .display()
                .to_string(),
        );
        cargo.to_bin = default_to_bin_for_target_config(target, plat_dyn);
        Ok(cargo)
    }

    pub(super) fn resolve_std_features(&mut self) {
        self.features = self
            .features
            .iter()
            .map(|feature| normalize_std_feature(feature))
            .collect();
        self.features.sort();
        self.features.dedup();
    }

    pub(super) fn resolve_std_features_with_metadata(
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

    pub(super) fn resolve_features_with_prefix_family(
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
        let mut args = vec!["-Z".to_string(), "build-std=core,alloc".to_string()];

        if !extra_rustflags.is_empty() {
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
        Self {
            env: HashMap::new(),
            log: LogLevel::Warn,
            features: vec!["ax-std".to_string()],
            max_cpu_num: None,
            axconfig_overrides: Vec::new(),
            plat_dyn: true,
        }
    }
}
