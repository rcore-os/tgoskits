use anyhow::bail;

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

/// Appends rustc arguments without changing Cargo's active rustflags source.
///
/// Environment sources stay environment sources. Target-specific config stays
/// target-specific so linker, relocation, and platform flags continue to merge
/// with command-specific test, coverage, or profiling arguments.
pub(crate) fn append_cargo_rustflags(cargo: &mut Cargo, flags: &[&str]) {
    const ENCODED_KEY: &str = "CARGO_ENCODED_RUSTFLAGS";
    const PLAIN_KEY: &str = "RUSTFLAGS";
    const BUILD_KEY: &str = "CARGO_BUILD_RUSTFLAGS";

    if flags.is_empty() {
        return;
    }

    // Cargo selects exactly one rustflags source. Extend the active environment
    // source when one exists; otherwise remain in the target-specific config
    // source so linker and platform flags continue to participate.
    if let Some(mut value) = effective_cargo_env(cargo, ENCODED_KEY) {
        append_encoded_rustflag_sequence(&mut value, flags);
        cargo.env.insert(ENCODED_KEY.to_string(), value);
        return;
    }

    if let Some(value) = effective_cargo_env(cargo, PLAIN_KEY) {
        let mut active_flags = value.split_whitespace().map(ToOwned::to_owned).collect();
        append_rustflag_sequence(&mut active_flags, flags);
        cargo.env.remove(PLAIN_KEY);
        cargo
            .env
            .insert(ENCODED_KEY.to_string(), active_flags.join("\x1f"));
        return;
    }

    // Some host-side preparation tests and tools intentionally leave target
    // resolution to a later stage. Without a concrete target, a target config
    // key would be invalid; retain the legacy encoded-environment fallback.
    if cargo.target.is_empty() {
        let mut value = String::new();
        append_encoded_rustflag_sequence(&mut value, flags);
        cargo.env.insert(ENCODED_KEY.to_string(), value);
        return;
    }

    let target_key = cargo_target_key(cargo);
    let target_scope = RustflagsScope::Target(&target_key);
    if append_inline_rustflags(cargo, target_scope, flags) {
        return;
    }

    let target_env = cargo_target_rustflags_env(&target_key);
    if append_plain_env_rustflags(cargo, &target_env, target_scope, flags) {
        return;
    }

    if let Some(active) = extra_config_rustflags(cargo, target_scope) {
        append_config_rustflags_overlay(cargo, target_scope, Some(active), flags);
        return;
    }

    if append_plain_env_rustflags(cargo, BUILD_KEY, RustflagsScope::Build, flags) {
        return;
    }

    let build_scope = RustflagsScope::Build;
    if append_inline_rustflags(cargo, build_scope, flags) {
        return;
    }

    if let Some(active) = extra_config_rustflags(cargo, build_scope) {
        append_config_rustflags_overlay(cargo, build_scope, Some(active), flags);
        return;
    }

    append_config_rustflags_overlay(cargo, target_scope, None, flags);
}

fn effective_cargo_env(cargo: &Cargo, key: &str) -> Option<String> {
    cargo
        .env
        .get(key)
        .cloned()
        .or_else(|| std::env::var(key).ok())
}

fn cargo_target_key(cargo: &Cargo) -> String {
    Path::new(&cargo.target)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(&cargo.target)
        .to_string()
}

fn cargo_target_rustflags_env(target_key: &str) -> String {
    let target_key = target_key
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("CARGO_TARGET_{target_key}_RUSTFLAGS")
}

fn append_plain_env_rustflags(
    cargo: &mut Cargo,
    key: &str,
    scope: RustflagsScope<'_>,
    flags: &[&str],
) -> bool {
    let Some(value) = effective_cargo_env(cargo, key) else {
        return false;
    };
    let mut active_flags = value.split_whitespace().map(ToOwned::to_owned).collect();
    if !append_rustflag_sequence(&mut active_flags, flags) {
        return true;
    }
    if flags
        .iter()
        .any(|flag| flag.is_empty() || flag.chars().any(char::is_whitespace))
    {
        // Cargo's target/build environment variables are whitespace-split.
        // Keep a rustc argument containing spaces intact in a merging config
        // array rather than corrupting it while rewriting the environment.
        append_config_rustflags_overlay(cargo, scope, None, flags);
    } else {
        cargo.env.insert(key.to_string(), active_flags.join(" "));
    }
    true
}

#[derive(Clone, Copy)]
enum RustflagsScope<'a> {
    Target(&'a str),
    Build,
}

impl RustflagsScope<'_> {
    fn find(self, table: &toml::Table) -> Option<&toml::Value> {
        match self {
            Self::Target(target_key) => table
                .get("target")?
                .as_table()?
                .get(target_key)?
                .as_table()?
                .get("rustflags"),
            Self::Build => table.get("build")?.as_table()?.get("rustflags"),
        }
    }

    fn assignment_key(self) -> String {
        match self {
            Self::Target(target_key) => {
                let target_key = toml::Value::String(target_key.to_string());
                format!("target.{target_key}.rustflags")
            }
            Self::Build => "build.rustflags".to_string(),
        }
    }
}

fn append_inline_rustflags(cargo: &mut Cargo, scope: RustflagsScope<'_>, flags: &[&str]) -> bool {
    for index in (0..cargo.args.len()).rev() {
        if let Some(assignment) = cargo.args[index].strip_prefix("--config=") {
            let Some(updated) = append_rustflags_assignment(assignment, scope, flags) else {
                continue;
            };
            cargo.args[index] = format!("--config={updated}");
            return true;
        }

        if index == 0 || cargo.args[index - 1] != "--config" {
            continue;
        }
        let Some(updated) = append_rustflags_assignment(&cargo.args[index], scope, flags) else {
            continue;
        };
        cargo.args[index] = updated;
        return true;
    }

    false
}

fn append_rustflags_assignment(
    assignment: &str,
    scope: RustflagsScope<'_>,
    flags: &[&str],
) -> Option<String> {
    let (raw_key, _) = assignment.split_once('=')?;
    let table = toml::from_str::<toml::Table>(assignment).ok()?;
    let mut rustflags = RustflagsValue::parse(scope.find(&table)?)?;
    append_rustflag_sequence(&mut rustflags.flags, flags);
    let value = rustflags.render();
    Some(format!("{}={value}", raw_key.trim()))
}

#[derive(Clone, Copy)]
enum RustflagsFormat {
    Array,
    String,
}

struct RustflagsValue {
    flags: Vec<String>,
    format: RustflagsFormat,
}

impl RustflagsValue {
    fn parse(value: &toml::Value) -> Option<Self> {
        match value {
            toml::Value::Array(rustflags) => Some(Self {
                flags: rustflags
                    .iter()
                    .map(|flag| flag.as_str().map(ToOwned::to_owned))
                    .collect::<Option<Vec<_>>>()?,
                format: RustflagsFormat::Array,
            }),
            toml::Value::String(rustflags) => Some(Self {
                flags: rustflags
                    .split_whitespace()
                    .map(ToOwned::to_owned)
                    .collect(),
                format: RustflagsFormat::String,
            }),
            _ => None,
        }
    }

    fn render(self) -> toml::Value {
        match self.format {
            RustflagsFormat::Array => {
                toml::Value::Array(self.flags.into_iter().map(toml::Value::String).collect())
            }
            RustflagsFormat::String => toml::Value::String(self.flags.join(" ")),
        }
    }
}

fn extra_config_rustflags(cargo: &Cargo, scope: RustflagsScope<'_>) -> Option<RustflagsValue> {
    let config = cargo.extra_config.as_deref()?;
    if config.starts_with("http://") || config.starts_with("https://") {
        return None;
    }
    let source = fs::read_to_string(config).ok()?;
    let table = toml::from_str::<toml::Table>(&source).ok()?;
    RustflagsValue::parse(scope.find(&table)?)
}

fn append_config_rustflags_overlay(
    cargo: &mut Cargo,
    scope: RustflagsScope<'_>,
    active: Option<RustflagsValue>,
    flags: &[&str],
) {
    let value = match active {
        Some(mut active) => {
            if !append_rustflag_sequence(&mut active.flags, flags) {
                return;
            }
            match active.format {
                // Cargo merges rustflags arrays from multiple config layers,
                // so repeat only the new sequence in the command-line layer.
                RustflagsFormat::Array => RustflagsValue {
                    flags: flags.iter().map(|flag| (*flag).to_string()).collect(),
                    format: RustflagsFormat::Array,
                }
                .render(),
                // Strings replace rather than merge; carry the active value
                // forward when adding the command-line override.
                RustflagsFormat::String => active.render(),
            }
        }
        None => RustflagsValue {
            flags: flags.iter().map(|flag| (*flag).to_string()).collect(),
            format: RustflagsFormat::Array,
        }
        .render(),
    };
    cargo.args.push("--config".to_string());
    cargo
        .args
        .push(format!("{}={value}", scope.assignment_key()));
}

fn append_encoded_rustflag_sequence(value: &mut String, flags: &[&str]) {
    let mut active_flags = if value.is_empty() {
        Vec::new()
    } else {
        value.split('\x1f').map(ToOwned::to_owned).collect()
    };
    if append_rustflag_sequence(&mut active_flags, flags) {
        *value = active_flags.join("\x1f");
    }
}

fn append_rustflag_sequence(active_flags: &mut Vec<String>, flags: &[&str]) -> bool {
    if active_flags
        .windows(flags.len())
        .any(|window| window.iter().map(String::as_str).eq(flags.iter().copied()))
    {
        return false;
    }
    active_flags.extend(flags.iter().map(|flag| (*flag).to_string()));
    true
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

/// Link contract for freestanding kernels built without Rust `std`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BareKernelLinkMode {
    /// Use the target's default relocation and linker policy.
    Default,
    /// Produce a position-independent executable with the kernel linker script.
    Pie,
}

impl BareKernelLinkMode {
    fn rustflags(self, target: &str) -> Vec<String> {
        match self {
            Self::Default => Vec::new(),
            Self::Pie => {
                let mut flags = vec![
                    "-Crelocation-model=pic".to_string(),
                    "-Clink-args=-pie".to_string(),
                ];
                if target.starts_with("riscv64") {
                    flags.push("-Clink-args=--no-relax".to_string());
                }
                flags.extend([
                    "-Clink-args=--gc-sections".to_string(),
                    "-Clink-args=-znorelro".to_string(),
                    "-Clink-args=-znostart-stop-gc".to_string(),
                    "-Clink-args=-Tlinker.x".to_string(),
                    "-Clink-args=-u _head".to_string(),
                ]);
                flags
            }
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
        // Keep the Cargo artifact as ELF by default. BIN conversion is an
        // explicit runner/config concern and must not be inferred from target.
        self.into_base_cargo_config_with_to_bin(package, target, args, false)
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
        self.validate_features()?;
        self.resolve_std_features();
        // `max_cpu_num` is an explicit build setting. Propagate SMP only when
        // the caller requested more than one CPU; package metadata never adds
        // features implicitly.
        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push("smp".to_string());
            self.resolve_std_features();
        }
        let std_target = std_build_target_for(target)?;
        let fake_lib_dir = std_fake_lib_dir(&std_target.target_name)?;
        let wrapper = std_linker_wrapper_path(&std_target.target_name, &fake_lib_dir)?;
        let mut cargo = self.into_base_cargo_config_with_log(
            package.to_string(),
            std_target.target.clone(),
            std_target.cargo_args,
        );
        cargo.env.extend(std_target.env);
        // The std target wrapper needs the original kernel target. This is
        // build context, not a Cargo feature or platform selection.
        cargo
            .env
            .insert("AX_TARGET".to_string(), target.to_string());
        let app_features = package_feature_names(package, metadata)?;
        let axstd_features = package_feature_names(AXSTD_STD_PACKAGE, metadata)?;
        pass_std_build_nested_features(&mut cargo.features, &app_features, &axstd_features);
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
        Ok(cargo)
    }

    /// Builds a Rust-`std` kernel through the musl PIE target and linker wrapper.
    pub(crate) fn into_prepared_std_cargo_config_with_metadata(
        self,
        package: &str,
        target: &str,
        metadata: &Metadata,
    ) -> anyhow::Result<Cargo> {
        self.into_prepared_base_cargo_config_with_metadata(package, target, metadata)
    }

    /// Builds a freestanding kernel against only `core` and `alloc`.
    pub(crate) fn into_prepared_no_std_cargo_config_with_metadata(
        mut self,
        package: &str,
        target: &str,
        metadata: &Metadata,
        link_mode: BareKernelLinkMode,
    ) -> anyhow::Result<Cargo> {
        self.validated_max_cpu_num()?;
        self.validate_features()?;
        self.reject_freestanding_std_compat()?;
        self.enable_package_smp_feature(package, metadata)?;

        let mut rustflags = toolchain_rustflags_for_features(&self.env, &self.features);
        rustflags.extend(link_mode.rustflags(target));
        let bare_target = freestanding_build_target_for(target);
        let args = Self::build_cargo_args(target, &rustflags);
        let mut cargo =
            self.into_base_cargo_config_with_log(package.to_string(), bare_target.target, args);
        cargo.env.extend(bare_target.env);
        cargo
            .env
            .insert("AX_TARGET".to_string(), target.to_string());
        cargo.to_bin = bare_target_requires_bin(target);
        Ok(cargo)
    }

    fn reject_freestanding_std_compat(&self) -> anyhow::Result<()> {
        if let Some(feature) = self
            .features
            .iter()
            .find(|feature| feature.rsplit('/').next() == Some("std-compat"))
        {
            bail!("freestanding no_std build cannot enable `{feature}`");
        }
        Ok(())
    }

    fn enable_package_smp_feature(
        &mut self,
        package: &str,
        metadata: &Metadata,
    ) -> anyhow::Result<()> {
        if !self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            return Ok(());
        }
        if package_feature_names(package, metadata)?
            .iter()
            .any(|feature| feature == "smp")
        {
            self.features.push("smp".to_string());
            self.features.sort();
            self.features.dedup();
        }
        Ok(())
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

    pub(crate) fn resolve_c_app_features(&mut self) -> anyhow::Result<()> {
        self.validate_features()?;
        // `max_cpu_num` is an explicit C build setting; expose the matching ax-std
        // capability only when the caller requested more than one CPU.
        if self.max_cpu_num.is_some_and(|max_cpu_num| max_cpu_num > 1) {
            self.features.push("ax-std/smp".to_string());
        }
        self.features.sort();
        self.features.dedup();
        Ok(())
    }

    /// Reject compatibility aliases and removed platform controls instead of silently changing
    /// the build contract selected by the caller.
    pub(crate) fn validate_features(&self) -> anyhow::Result<()> {
        let selects_mode = |mode: &str| {
            self.features
                .iter()
                .any(|feature| feature.rsplit('/').next() == Some(mode))
        };
        if selects_mode("uspace") && selects_mode("tls") {
            bail!(
                "features `uspace` and `tls` select incompatible CPU-local register ownership \
                 modes"
            );
        }
        for feature in &self.features {
            self.validate_feature(feature)?;
        }
        Ok(())
    }

    pub(crate) fn validate_feature(&self, feature: &str) -> anyhow::Result<()> {
        if feature == "axstd" || feature.starts_with("axstd/") {
            bail!(
                "feature `{feature}` uses the removed `axstd` alias; use the declared Cargo \
                 feature name instead"
            );
        }
        if is_removed_dynamic_platform_feature(feature) {
            bail!(
                "feature `{feature}` is no longer supported; dynamic platform selection is \
                 automatic, remove the feature from the selected configuration"
            );
        }
        Ok(())
    }

    pub(crate) fn validated_max_cpu_num(&self) -> anyhow::Result<Option<usize>> {
        match self.max_cpu_num {
            Some(0) => bail!("max_cpu_num must be greater than 0"),
            Some(max_cpu_num) => Ok(Some(max_cpu_num)),
            None => Ok(None),
        }
    }

    pub(crate) fn build_cargo_args(target: &str, extra_rustflags: &[String]) -> Vec<String> {
        let bare_target = freestanding_build_target_for(target);
        let mut args = bare_target.cargo_args;
        args.extend(Self::rustflags_cargo_args(
            &bare_target.target,
            extra_rustflags,
        ));
        args
    }

    fn rustflags_cargo_args(target: &str, extra_rustflags: &[String]) -> Vec<String> {
        let mut args = Vec::new();
        let target_key = Path::new(target)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(target);

        let rustflags = extra_rustflags.to_vec();

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

fn bare_target_requires_bin(target: &str) -> bool {
    target.starts_with("aarch64-") || target.starts_with("riscv64")
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
