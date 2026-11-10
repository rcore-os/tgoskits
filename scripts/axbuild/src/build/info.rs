use std::collections::VecDeque;

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

enum FeatureResolutionEvent {
    ActivatePackage {
        package: String,
        include_default: bool,
        features: Vec<String>,
    },
    EnableDependency {
        owner: String,
        dependency: String,
        feature: Option<String>,
    },
    EnableFeature {
        package: String,
        feature: String,
    },
}

fn dependency_for<'a>(package: &'a Package, name: &str) -> Option<&'a cargo_metadata::Dependency> {
    package.dependencies.iter().find(|dependency| {
        dependency.name == name
            || dependency.rename.as_deref() == Some(name)
            || (name == "axlibc" && dependency.name == "ax-libc")
    })
}

fn dependency_package_name(
    metadata: &Metadata,
    dependency: &cargo_metadata::Dependency,
) -> Option<String> {
    let candidates = [
        dependency.name.as_str(),
        dependency.rename.as_deref().unwrap_or(""),
    ];
    candidates.into_iter().find_map(|candidate| {
        metadata
            .packages
            .iter()
            .find(|package| {
                metadata.workspace_members.contains(&package.id) && package.name == candidate
            })
            .map(|package| package.name.to_string())
    })
}

fn feature_resolution_has_mmu(
    package: &str,
    metadata: &Metadata,
    requested_features: &[String],
    include_default_features: bool,
) -> anyhow::Result<bool> {
    let mut events = VecDeque::from([FeatureResolutionEvent::ActivatePackage {
        package: package.to_string(),
        include_default: include_default_features,
        features: requested_features.to_vec(),
    }]);
    let mut activated_packages = HashSet::new();
    let mut activated_dependencies = HashSet::new();
    let mut visited_features = HashSet::new();

    while let Some(event) = events.pop_front() {
        match event {
            FeatureResolutionEvent::ActivatePackage {
                package,
                include_default,
                features,
            } => {
                let package_info = workspace_package(metadata, &package)?;
                if activated_packages.insert(package.clone()) {
                    if include_default && package_info.features.contains_key("default") {
                        events.push_back(FeatureResolutionEvent::EnableFeature {
                            package: package.clone(),
                            feature: "default".to_string(),
                        });
                    }
                    for dependency in package_info
                        .dependencies
                        .iter()
                        .filter(|dependency| !dependency.optional)
                    {
                        events.push_back(FeatureResolutionEvent::EnableDependency {
                            owner: package.clone(),
                            dependency: dependency.name.clone(),
                            feature: None,
                        });
                    }
                }
                for feature in features {
                    if package_info.features.contains_key(&feature) {
                        events.push_back(FeatureResolutionEvent::EnableFeature {
                            package: package.clone(),
                            feature,
                        });
                    } else if let Some((dependency, feature)) = feature.split_once('/') {
                        events.push_back(FeatureResolutionEvent::EnableDependency {
                            owner: package.clone(),
                            dependency: dependency.to_string(),
                            feature: Some(feature.to_string()),
                        });
                    } else if dependency_for(package_info, &feature).is_some() {
                        events.push_back(FeatureResolutionEvent::EnableDependency {
                            owner: package.clone(),
                            dependency: feature,
                            feature: None,
                        });
                    }
                }
            }
            FeatureResolutionEvent::EnableDependency {
                owner,
                dependency,
                feature,
            } => {
                let owner_info = workspace_package(metadata, &owner)?;
                let optional_feature = dependency.ends_with('?');
                let dependency_name = dependency.trim_end_matches('?');
                let Some(dependency_info) = dependency_for(owner_info, dependency_name) else {
                    continue;
                };
                let dependency_key = (owner.clone(), dependency_name.to_string());
                if optional_feature && !activated_dependencies.contains(&dependency_key) {
                    continue;
                }
                let Some(dependency_package) = dependency_package_name(metadata, dependency_info)
                else {
                    continue;
                };
                if activated_dependencies.insert(dependency_key) {
                    events.push_back(FeatureResolutionEvent::ActivatePackage {
                        package: dependency_package.clone(),
                        include_default: dependency_info.uses_default_features,
                        features: dependency_info.features.clone(),
                    });
                }
                if let Some(feature) = feature {
                    events.push_back(FeatureResolutionEvent::EnableFeature {
                        package: dependency_package,
                        feature,
                    });
                }
            }
            FeatureResolutionEvent::EnableFeature { package, feature } => {
                if !visited_features.insert((package.clone(), feature.clone())) {
                    continue;
                }
                if package == "ax-runtime" && feature == "paging" {
                    return Ok(true);
                }
                let package_info = workspace_package(metadata, &package)?;
                let Some(values) = package_info.features.get(&feature) else {
                    continue;
                };
                for value in values {
                    if let Some(dependency) = value.strip_prefix("dep:") {
                        events.push_back(FeatureResolutionEvent::EnableDependency {
                            owner: package.clone(),
                            dependency: dependency.to_string(),
                            feature: None,
                        });
                    } else if let Some((dependency, feature)) = value.split_once('/') {
                        events.push_back(FeatureResolutionEvent::EnableDependency {
                            owner: package.clone(),
                            dependency: dependency.to_string(),
                            feature: Some(feature.to_string()),
                        });
                    } else if package_info.features.contains_key(value) {
                        events.push_back(FeatureResolutionEvent::EnableFeature {
                            package: package.clone(),
                            feature: value.clone(),
                        });
                    } else if dependency_for(package_info, value).is_some() {
                        events.push_back(FeatureResolutionEvent::EnableDependency {
                            owner: package.clone(),
                            dependency: value.clone(),
                            feature: None,
                        });
                    }
                }
            }
        }
    }
    Ok(false)
}

/// Returns whether Cargo's default and requested package features select an MMU-backed
/// ArceOS dependency. This closes the gap between Cargo's feature resolver and the
/// explicit `CommonConfig.features` list used to construct rustflags.
pub(crate) fn package_enables_mmu(
    package: &str,
    metadata: &Metadata,
    requested_features: &[String],
) -> anyhow::Result<bool> {
    package_enables_mmu_with_defaults(package, metadata, requested_features, true)
}

pub(crate) fn package_enables_mmu_with_defaults(
    package: &str,
    metadata: &Metadata,
    requested_features: &[String],
    include_default_features: bool,
) -> anyhow::Result<bool> {
    feature_resolution_has_mmu(
        package,
        metadata,
        requested_features,
        include_default_features,
    )
}

/// Returns whether the selected package feature graph enables the MMU provider.
pub(crate) fn package_mmu_enabled(
    package: &str,
    metadata: &Metadata,
    features: &[String],
    include_default_features: bool,
) -> anyhow::Result<bool> {
    package_enables_mmu_with_defaults(package, metadata, features, include_default_features)
}

fn package_mmu_feature_marker(
    package: &str,
    metadata: &Metadata,
) -> anyhow::Result<Option<String>> {
    let package = workspace_package(metadata, package)?;
    if package.features.contains_key("paging") {
        return Ok(Some("paging".to_string()));
    }
    for dependency in ["ax-std", "ax-runtime", "ax-hal", "ax-libc", "axlibc"] {
        if package
            .dependencies
            .iter()
            .any(|candidate| candidate.name == dependency)
        {
            return Ok(Some(format!("{dependency}/paging")));
        }
    }
    Ok(None)
}

pub(crate) fn toolchain_rustflags_for_mmu(
    env: &HashMap<String, String>,
    mmu_enabled: bool,
) -> Vec<String> {
    let mut flags = toolchain_rustflags(env);
    if mmu_enabled {
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

/// Converts the shared log level to the runner's configuration contract.
pub(crate) fn runner_log_level(log: LogLevel) -> ostool::build::config::LogLevel {
    match log {
        LogLevel::Trace => ostool::build::config::LogLevel::Trace,
        LogLevel::Debug => ostool::build::config::LogLevel::Debug,
        LogLevel::Info => ostool::build::config::LogLevel::Info,
        LogLevel::Warn => ostool::build::config::LogLevel::Warn,
        LogLevel::Error => ostool::build::config::LogLevel::Error,
    }
}

pub(crate) fn into_base_cargo_config(
    config: CommonConfig,
    package: String,
    target: String,
    args: Vec<String>,
    runtime: RuntimeValues,
) -> Cargo {
    let mut env: HashMap<_, _> = config.env.into_iter().collect();
    env.insert("AX_LOG".into(), format!("{:?}", config.log).to_lowercase());
    env.insert("SMP".into(), runtime.cpu_capacity.to_string());
    env.insert(
        "AX_TASK_STACK_SIZE".into(),
        runtime.task_stack_size.to_string(),
    );
    env.insert(
        "AX_SCHEDULER_TICK_MS".into(),
        runtime.scheduler_tick_ms.to_string(),
    );
    Cargo {
        env,
        target,
        package,
        features: config.features,
        log: Some(runner_log_level(config.log)),
        extra_config: None,
        profile: None,
        disable_someboot_build_config: true,
        args,
        pre_build_cmds: vec![],
        post_build_cmds: vec![],
        to_bin: false,
        bin: None,
        test: None,
    }
}

pub(crate) fn into_prepared_base_cargo_config_with_metadata(
    config: CommonConfig,
    package: &str,
    target: &str,
    metadata: &Metadata,
    axbuild_dir: &Path,
    runtime: RuntimeValues,
) -> anyhow::Result<Cargo> {
    into_prepared_base_cargo_config_with_metadata_and_defaults(
        config,
        package,
        target,
        metadata,
        axbuild_dir,
        true,
        runtime,
    )
}

pub(crate) fn into_prepared_base_cargo_config_with_metadata_and_defaults(
    mut config: CommonConfig,
    package: &str,
    target: &str,
    metadata: &Metadata,
    axbuild_dir: &Path,
    include_default_features: bool,
    runtime: RuntimeValues,
) -> anyhow::Result<Cargo> {
    normalize_mmu_feature_aliases(&mut config, package, metadata)?;
    let mmu_enabled = package_mmu_enabled(
        package,
        metadata,
        &config.features,
        include_default_features,
    )?;
    config.validate()?;
    validate_features(&config)?;
    resolve_std_features(&mut config);
    // `max_cpu_num` is an explicit build setting. Propagate SMP only when
    // the caller requested more than one CPU; package metadata never adds
    // features implicitly.
    if config
        .max_cpu_num
        .is_some_and(|max_cpu_num| max_cpu_num > 1)
    {
        config.features.push("smp".to_string());
        resolve_std_features(&mut config);
    }
    let std_target = std_build_target_for(target)?;
    let fake_lib_dir = std_fake_lib_dir(axbuild_dir, &std_target.target_name)?;
    let wrapper = std_linker_wrapper_path(axbuild_dir, &std_target.target_name, &fake_lib_dir)?;
    let extra_rustflags = std::mem::take(&mut config.rustflags);
    let mut cargo = into_base_cargo_config(
        config,
        package.to_string(),
        std_target.target.clone(),
        std_target.cargo_args,
        runtime,
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
        std_fake_lib_prebuild_script_path(
            axbuild_dir,
            &std_target.target_name,
            &fake_lib_dir,
            &cargo.env,
        )?
        .display()
        .to_string(),
    );
    let rustflags = toolchain_rustflags_for_mmu(&cargo.env, mmu_enabled);
    cargo.extra_config = Some(
        std_cargo_config_path(axbuild_dir, &std_target.target_name, &wrapper, &rustflags)?
            .display()
            .to_string(),
    );
    append_cargo_rustflags(
        &mut cargo,
        &extra_rustflags
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    Ok(cargo)
}

/// Builds a Rust-`std` kernel through the musl PIE target and linker wrapper.
pub(crate) fn into_prepared_std_cargo_config_with_metadata(
    config: CommonConfig,
    package: &str,
    target: &str,
    metadata: &Metadata,
    axbuild_dir: &Path,
    runtime: RuntimeValues,
) -> anyhow::Result<Cargo> {
    into_prepared_base_cargo_config_with_metadata(
        config,
        package,
        target,
        metadata,
        axbuild_dir,
        runtime,
    )
}

pub(crate) fn into_prepared_std_cargo_config_with_metadata_and_defaults(
    config: CommonConfig,
    package: &str,
    target: &str,
    metadata: &Metadata,
    axbuild_dir: &Path,
    include_default_features: bool,
    runtime: RuntimeValues,
) -> anyhow::Result<Cargo> {
    into_prepared_base_cargo_config_with_metadata_and_defaults(
        config,
        package,
        target,
        metadata,
        axbuild_dir,
        include_default_features,
        runtime,
    )
}

/// Builds a freestanding kernel against only `core` and `alloc`.
pub(crate) fn into_prepared_no_std_cargo_config_with_metadata(
    config: CommonConfig,
    package: &str,
    target: &str,
    metadata: &Metadata,
    link_mode: BareKernelLinkMode,
    runtime: RuntimeValues,
) -> anyhow::Result<Cargo> {
    into_prepared_no_std_cargo_config_with_metadata_and_defaults(
        config, package, target, metadata, link_mode, true, runtime,
    )
}

pub(crate) fn into_prepared_no_std_cargo_config_with_metadata_and_defaults(
    mut config: CommonConfig,
    package: &str,
    target: &str,
    metadata: &Metadata,
    link_mode: BareKernelLinkMode,
    include_default_features: bool,
    runtime: RuntimeValues,
) -> anyhow::Result<Cargo> {
    normalize_mmu_feature_aliases(&mut config, package, metadata)?;
    let mmu_enabled = package_mmu_enabled(
        package,
        metadata,
        &config.features,
        include_default_features,
    )?;
    config.validate()?;
    validate_features(&config)?;
    reject_freestanding_std_compat(&config)?;
    enable_package_smp_feature(&mut config, package, metadata)?;

    let build_env = config
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut rustflags = toolchain_rustflags_for_mmu(&build_env, mmu_enabled);
    rustflags.extend(link_mode.rustflags(target));
    let bare_target = freestanding_build_target_for(target);
    let args = build_cargo_args(target, &rustflags);
    let extra_rustflags = std::mem::take(&mut config.rustflags);
    let mut cargo = into_base_cargo_config(
        config,
        package.to_string(),
        bare_target.target,
        args,
        runtime,
    );
    cargo.env.extend(bare_target.env);
    cargo
        .env
        .insert("AX_TARGET".to_string(), target.to_string());
    cargo.to_bin = bare_target_requires_bin(target);
    append_cargo_rustflags(
        &mut cargo,
        &extra_rustflags
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    Ok(cargo)
}

fn reject_freestanding_std_compat(config: &CommonConfig) -> anyhow::Result<()> {
    if let Some(feature) = config
        .features
        .iter()
        .find(|feature| feature.rsplit('/').next() == Some("std-compat"))
    {
        bail!("freestanding no_std build cannot enable `{feature}`");
    }
    Ok(())
}

fn enable_package_smp_feature(
    config: &mut CommonConfig,
    package: &str,
    metadata: &Metadata,
) -> anyhow::Result<()> {
    if !config
        .max_cpu_num
        .is_some_and(|max_cpu_num| max_cpu_num > 1)
    {
        return Ok(());
    }
    if package_feature_names(package, metadata)?
        .iter()
        .any(|feature| feature == "smp")
    {
        config.features.push("smp".to_string());
        config.features.sort();
        config.features.dedup();
    }
    Ok(())
}

pub(super) fn resolve_std_features(config: &mut CommonConfig) {
    config.features = config
        .features
        .iter()
        .map(|feature| normalize_std_feature(feature))
        .collect();
    config.features.sort();
    config.features.dedup();
}

fn normalize_mmu_feature_aliases(
    config: &mut CommonConfig,
    package: &str,
    metadata: &Metadata,
) -> anyhow::Result<()> {
    if !config
        .features
        .iter()
        .any(|feature| feature == "axlibc/paging")
    {
        return Ok(());
    }

    let marker = package_mmu_feature_marker(package, metadata)?.ok_or_else(|| {
        anyhow::anyhow!(
            "feature `axlibc/paging` cannot be normalized for package `{package}`; use a package \
             with a paging capability"
        )
    })?;
    for feature in &mut config.features {
        if feature == "axlibc/paging" {
            *feature = marker.clone();
        }
    }
    Ok(())
}

pub(crate) fn resolve_c_app_features(config: &mut CommonConfig) -> anyhow::Result<()> {
    validate_features(config)?;
    // `max_cpu_num` is an explicit C build setting; expose the matching ax-std
    // capability only when the caller requested more than one CPU.
    if config
        .max_cpu_num
        .is_some_and(|max_cpu_num| max_cpu_num > 1)
    {
        config.features.push("ax-std/smp".to_string());
    }
    config.features.sort();
    config.features.dedup();
    Ok(())
}

/// Reject compatibility aliases and removed platform controls instead of silently changing
/// the build contract selected by the caller.
pub(crate) fn validate_features(config: &CommonConfig) -> anyhow::Result<()> {
    let selects_mode = |mode: &str| {
        config
            .features
            .iter()
            .any(|feature| feature.rsplit('/').next() == Some(mode))
    };
    if selects_mode("uspace") && selects_mode("tls") {
        bail!("features `uspace` and `tls` select incompatible CPU-local register ownership modes");
    }
    for feature in &config.features {
        validate_feature(feature)?;
    }
    Ok(())
}

pub(crate) fn validate_feature(feature: &str) -> anyhow::Result<()> {
    if feature == "axstd" || feature.starts_with("axstd/") {
        bail!(
            "feature `{feature}` uses the removed `axstd` alias; use the declared Cargo feature \
             name instead"
        );
    }
    if is_removed_dynamic_platform_feature(feature) {
        bail!(
            "feature `{feature}` is no longer supported; dynamic platform selection is automatic, \
             remove the feature from the selected configuration"
        );
    }
    if is_removed_stack_hardening_feature(feature) {
        bail!(
            "feature `{feature}` was removed; enable the canonical `paging` capability \
             (`ax-std/paging` for system builds) instead"
        );
    }
    Ok(())
}

pub(crate) fn build_cargo_args(target: &str, extra_rustflags: &[String]) -> Vec<String> {
    let bare_target = freestanding_build_target_for(target);
    let mut args = bare_target.cargo_args;
    let mut rustflags = bare_target.rustflags;
    rustflags.extend_from_slice(extra_rustflags);
    args.extend(rustflags_cargo_args(&bare_target.target, &rustflags));
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
fn bare_target_requires_bin(target: &str) -> bool {
    target.starts_with("aarch64-") || target.starts_with("riscv64")
}
