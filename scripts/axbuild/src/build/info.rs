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
            "stack-protector" | "ax-std/stack-protector" | "starry-kernel/stack-protector"
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
    let encoded = flags.join("\x1f");
    if encoded.is_empty() {
        return;
    }
    let value = cargo.env.entry(KEY.to_string()).or_default();
    if encoded_rustflags_contains_sequence(value, &encoded) {
        return;
    }
    if !value.is_empty() {
        value.push('\x1f');
    }
    value.push_str(&encoded);
}

fn encoded_rustflags_contains_sequence(value: &str, encoded: &str) -> bool {
    let needle: Vec<_> = encoded.split('\x1f').collect();
    if needle.is_empty() {
        return true;
    }
    value
        .split('\x1f')
        .collect::<Vec<_>>()
        .windows(needle.len())
        .any(|window| window == needle.as_slice())
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
pub(super) enum StdFeaturePrefixFamily {
    AxStd,
}

impl StdFeaturePrefixFamily {
    fn prefix(self) -> &'static str {
        match self {
            Self::AxStd => "ax-std/",
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
            test: None,
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
        metadata: &Metadata,
    ) -> anyhow::Result<Cargo> {
        self.validated_max_cpu_num()?;
        self.resolve_std_features_with_metadata(package, target, metadata);
        let std_target = std_build_target_for(target)?;
        let fake_lib_dir = std_fake_lib_dir(&std_target.target_name)?;
        let wrapper = std_linker_wrapper_path(&std_target.target_name, &fake_lib_dir)?;
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
            &cargo.features,
            metadata,
        )?;
        let app_features = package_feature_names(package, metadata)?;
        let axstd_features = package_feature_names(AXSTD_STD_PACKAGE, metadata)?;
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
            std_cargo_config_path(&std_target.target_name, &wrapper, &rustflags)?
                .display()
                .to_string(),
        );
        cargo.to_bin = true;
        Ok(cargo)
    }

    pub(super) fn resolve_std_features(&mut self) {
        self.features = self
            .features
            .iter()
            .map(|feature| normalize_std_feature(feature))
            .filter(|feature| !is_removed_dynamic_platform_feature(feature))
            .collect();
        self.features.sort();
        self.features.dedup();
    }

    pub(super) fn resolve_std_features_with_metadata(
        &mut self,
        _package: &str,
        _target: &str,
        _metadata: &Metadata,
    ) {
        self.resolve_std_features();

        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push("smp".to_string());
        }
        self.resolve_std_features();
    }

    pub(crate) fn resolve_features_with_metadata(
        &mut self,
        package: &str,
        target: &str,
        metadata: &Metadata,
    ) {
        self.resolve_features_with_prefix_family(
            package,
            target,
            detect_std_feature_prefix_family(package, metadata),
            Some(metadata),
        );
    }

    pub(super) fn resolve_features_with_prefix_family(
        &mut self,
        package: &str,
        target: &str,
        prefix_family: anyhow::Result<StdFeaturePrefixFamily>,
        metadata: Option<&Metadata>,
    ) {
        let prefix_family = self.resolve_std_feature_prefix_family(package, prefix_family);
        let _ = (target, metadata);

        self.features
            .retain(|feature| !matches!(feature.as_str(), "plat-dyn" | "ax-std/plat-dyn"));

        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push(format!("{}smp", prefix_family.prefix()));
        }

        self.features.sort();
        self.features.dedup();
    }

    fn resolve_std_feature_prefix_family(
        &self,
        package: &str,
        prefix_family: anyhow::Result<StdFeaturePrefixFamily>,
    ) -> StdFeaturePrefixFamily {
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
                StdFeaturePrefixFamily::AxStd
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
    pub(crate) fn resolve_features(&mut self, package: &str, target: &str) {
        match workspace_metadata() {
            Ok(metadata) => self.resolve_features_with_metadata(package, target, &metadata),
            Err(err) => self.resolve_features_with_prefix_family(
                package,
                target,
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
        let target_key = Path::new(target)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(target);

        let mut rustflags = extra_rustflags.to_vec();
        if target_key.starts_with("loongarch64-") {
            rustflags.push("-Ctarget-feature=-ual".to_string());
        }

        if !rustflags.is_empty() {
            args.push("--config".to_string());
            let rustflags_toml =
                toml::Value::Array(rustflags.into_iter().map(toml::Value::String).collect())
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
            features: Vec::new(),
            max_cpu_num: None,
        }
    }
}
