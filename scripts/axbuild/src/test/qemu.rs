use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use ostool::run::qemu::QemuConfig;
use serde::Deserialize;

use crate::{context::validate_supported_target, test::case::TestQemuCase};

const TIMEOUT_SCALE_ENV: &str = "AXBUILD_TEST_TIMEOUT_SCALE";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestBuildWrapper {
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    pub(crate) build_config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiscoveredQemuCase {
    pub(crate) name: String,
    pub(crate) display_name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) qemu_config_path: PathBuf,
    pub(crate) build_group: String,
    pub(crate) build_config_path: PathBuf,
}

pub(crate) struct QemuCaseGroup<'a, T> {
    pub(crate) build_group: &'a str,
    pub(crate) build_config_path: &'a Path,
    pub(crate) cases: Vec<&'a T>,
}

pub(crate) trait BuildConfigRef {
    fn build_group(&self) -> &str;
    fn build_config_path(&self) -> &Path;
}

pub(crate) fn resolve_named_test_config_path(
    app_dir: &Path,
    filename: &str,
    config_kind: &str,
) -> anyhow::Result<PathBuf> {
    let path = app_dir.join(filename);
    if path.exists() {
        return Ok(path);
    }

    let legacy_path = app_dir.join(format!(".{filename}"));
    if legacy_path.exists() {
        bail!(
            "unsupported legacy {config_kind} config `{}` under {}; expected `{filename}`",
            legacy_path.display(),
            app_dir.display()
        );
    }

    bail!(
        "missing {config_kind} config `{filename}` under {}",
        app_dir.display()
    )
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
struct RustQemuRuleConfig {
    #[serde(default)]
    default_qemu_config: Option<String>,
    #[serde(default, rename = "rule")]
    rules: Vec<RustQemuRule>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct RustQemuRule {
    qemu_config: String,
    #[serde(default)]
    when_features_any: Vec<String>,
    #[serde(default)]
    when_features_all: Vec<String>,
    #[serde(default)]
    when_features_none: Vec<String>,
}

impl RustQemuRule {
    fn matches(&self, features: &[String]) -> bool {
        let any_matches = self.when_features_any.is_empty()
            || self
                .when_features_any
                .iter()
                .any(|feature| feature_matches(features, feature));
        let all_matches = self
            .when_features_all
            .iter()
            .all(|feature| feature_matches(features, feature));
        let none_matches = self
            .when_features_none
            .iter()
            .all(|feature| !feature_matches(features, feature));
        any_matches && all_matches && none_matches
    }
}

pub(crate) fn resolve_rust_qemu_config_filename(
    app_dir: &Path,
    arch: &str,
    target: &str,
    features: &[String],
) -> anyhow::Result<String> {
    let default = format!("qemu-{arch}.toml");
    let Some(config_path) = resolve_optional_qemu_rule_config_path(app_dir)? else {
        return Ok(default);
    };

    let config: RustQemuRuleConfig = toml::from_str(
        &std::fs::read_to_string(&config_path)
            .with_context(|| format!("failed to read {}", config_path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", config_path.display()))?;

    for rule in &config.rules {
        if rule.matches(features) {
            return Ok(expand_qemu_config_template(&rule.qemu_config, arch, target));
        }
    }

    Ok(config
        .default_qemu_config
        .as_deref()
        .map(|template| expand_qemu_config_template(template, arch, target))
        .unwrap_or(default))
}

fn resolve_optional_qemu_rule_config_path(app_dir: &Path) -> anyhow::Result<Option<PathBuf>> {
    let path = app_dir.join("qemu-test.toml");
    if path.exists() {
        return Ok(Some(path));
    }

    let legacy_path = app_dir.join(".qemu-test.toml");
    if legacy_path.exists() {
        bail!(
            "unsupported legacy qemu rule config `{}` under {}; expected `qemu-test.toml`",
            legacy_path.display(),
            app_dir.display()
        );
    }

    Ok(None)
}

fn expand_qemu_config_template(template: &str, arch: &str, target: &str) -> String {
    template.replace("{arch}", arch).replace("{target}", target)
}

fn feature_matches(features: &[String], expected: &str) -> bool {
    features
        .iter()
        .any(|feature| feature == expected || feature.rsplit('/').next() == Some(expected))
}

pub(crate) fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

pub(crate) fn resolve_build_config_path(
    dir: &Path,
    target: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let path = dir.join(format!("build-{target}.toml"));
    if path.is_file() {
        return Ok(Some(path));
    }

    let legacy_candidates = legacy_build_config_candidates(dir, target);
    if !legacy_candidates.is_empty() {
        bail!(
            "unsupported legacy build config name(s) under {}: {}; expected only \
             `build-{target}.toml`",
            dir.display(),
            legacy_candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    Ok(None)
}

fn legacy_build_config_candidates(dir: &Path, target: &str) -> Vec<PathBuf> {
    let Some(arch) = arch_from_target_name(target) else {
        return Vec::new();
    };
    [
        dir.join(format!(".build-{target}.toml")),
        dir.join(format!("build-{arch}.toml")),
        dir.join(format!(".build-{arch}.toml")),
    ]
    .into_iter()
    .filter(|path| path.is_file())
    .collect()
}

fn arch_from_target_name(target: &str) -> Option<&str> {
    target.split_once('-').map(|(arch, _)| arch)
}

pub(crate) fn discover_build_wrappers(
    test_group_dir: &Path,
    target: &str,
    suite_name: &str,
    group_kind: &str,
) -> anyhow::Result<Vec<TestBuildWrapper>> {
    let mut wrappers = Vec::new();
    let mut stack = fs::read_dir(test_group_dir)
        .with_context(|| format!("failed to read {}", test_group_dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        if let Some(build_config_path) = resolve_build_config_path(&dir, target)? {
            wrappers.push(TestBuildWrapper {
                name: relative_case_name(test_group_dir, &dir)?,
                dir,
                build_config_path,
            });
            continue;
        }

        if is_case_asset_dir(&dir) {
            continue;
        }

        stack.extend(
            fs::read_dir(&dir)
                .with_context(|| format!("failed to read {}", dir.display()))?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }

    wrappers.sort_by(|left, right| left.name.cmp(&right.name));
    if wrappers.is_empty() {
        bail!(
            "no {suite_name} {group_kind} build wrappers for target `{target}` found under {}; \
             expected build-{target}.toml in a wrapper directory",
            test_group_dir.display()
        );
    }

    Ok(wrappers)
}

pub(crate) fn discover_all_qemu_cases(
    test_group_dir: &Path,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> anyhow::Result<Vec<String>> {
    if let Some(case_name) = selected_case {
        validate_selected_case_name(case_name, suite_name, group_label)?;
    }

    let mut by_name = BTreeMap::new();
    let mut stack = fs::read_dir(test_group_dir)
        .with_context(|| format!("failed to read {}", test_group_dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        let build_configs = build_config_paths(&dir)?;
        if !build_configs.is_empty() {
            collect_all_qemu_cases_in_build_wrapper(
                &TestBuildWrapper {
                    name: relative_case_name(test_group_dir, &dir)?,
                    dir,
                    build_config_path: build_configs[0].clone(),
                },
                selected_case,
                &mut by_name,
            )?;
            continue;
        }

        if is_case_asset_dir(&dir) {
            continue;
        }

        stack.extend(
            fs::read_dir(&dir)
                .with_context(|| format!("failed to read {}", dir.display()))?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }

    let cases = by_name.into_keys().collect::<Vec<_>>();
    if cases.is_empty() {
        if let Some(case_name) = selected_case {
            bail!(
                "unknown {suite_name} {group_label} test case `{case_name}` under {}; cases are \
                 discovered from build wrapper directories with qemu-*.toml",
                test_group_dir.display()
            );
        }
        bail!(
            "no {suite_name} {group_label} qemu test cases found under {}",
            test_group_dir.display()
        );
    }
    Ok(cases)
}

fn collect_all_qemu_cases_in_build_wrapper(
    build_wrapper: &TestBuildWrapper,
    selected_case: Option<&str>,
    cases: &mut BTreeMap<String, ()>,
) -> anyhow::Result<()> {
    if dir_contains_qemu_config(&build_wrapper.dir)?
        && selected_case.is_none_or(|case_name| case_name == build_wrapper.name)
    {
        cases.insert(build_wrapper.name.clone(), ());
    }

    let mut stack = fs::read_dir(&build_wrapper.dir)
        .with_context(|| format!("failed to read {}", build_wrapper.dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }

        if !case_dir_matches_selected(&build_wrapper.dir, &case_dir, selected_case)? {
            continue;
        }

        if !build_config_paths(&case_dir)?.is_empty() {
            continue;
        }

        if dir_contains_qemu_config(&case_dir)? {
            cases.insert(relative_case_name(&build_wrapper.dir, &case_dir)?, ());
            continue;
        }

        if is_case_asset_dir(&case_dir) {
            continue;
        }

        stack.extend(
            fs::read_dir(&case_dir)
                .with_context(|| format!("failed to read {}", case_dir.display()))?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }

    Ok(())
}

fn case_dir_matches_selected(
    build_wrapper_dir: &Path,
    case_dir: &Path,
    selected_case: Option<&str>,
) -> anyhow::Result<bool> {
    let Some(selected_case) = selected_case else {
        return Ok(true);
    };
    let case_name = relative_case_name(build_wrapper_dir, case_dir)?;
    Ok(selected_case == case_name || selected_case.starts_with(&format!("{case_name}/")))
}

fn dir_contains_qemu_config(dir: &Path) -> anyhow::Result<bool> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        if path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.starts_with("qemu-"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn nearest_build_wrapper(
    test_group_dir: &Path,
    case_dir: &Path,
    suite_name: &str,
    group_kind: &str,
) -> anyhow::Result<TestBuildWrapper> {
    let mut dir = case_dir;
    loop {
        let build_configs = build_config_paths(dir)?;
        match build_configs.as_slice() {
            [build_config_path] => {
                return Ok(TestBuildWrapper {
                    name: relative_case_name(test_group_dir, dir)?,
                    dir: dir.to_path_buf(),
                    build_config_path: build_config_path.clone(),
                });
            }
            [] => {}
            _ => bail!(
                "{suite_name} {group_kind} build wrapper `{}` provides multiple build-*.toml \
                 configs; wrappers must have exactly one build config when target is inferred",
                dir.display()
            ),
        }

        if dir == test_group_dir {
            bail!(
                "{suite_name} {group_kind} test case `{}` is not under a build wrapper with \
                 build-*.toml",
                case_dir.display()
            );
        }
        dir = dir.parent().with_context(|| {
            format!(
                "failed to find parent while resolving build wrapper for {}",
                case_dir.display()
            )
        })?;
    }
}

pub(crate) fn nearest_target_build_wrapper(
    test_group_dir: &Path,
    case_dir: &Path,
    target: &str,
    suite_name: &str,
    group_kind: &str,
) -> anyhow::Result<TestBuildWrapper> {
    let mut dir = case_dir;
    loop {
        if let Some(build_config_path) = resolve_build_config_path(dir, target)? {
            return Ok(TestBuildWrapper {
                name: relative_case_name(test_group_dir, dir)?,
                dir: dir.to_path_buf(),
                build_config_path,
            });
        }

        if dir == test_group_dir {
            bail!(
                "{suite_name} {group_kind} test case `{}` is not under a build wrapper with \
                 build-{target}.toml",
                case_dir.display()
            );
        }
        dir = dir.parent().with_context(|| {
            format!(
                "failed to find parent while resolving build wrapper for {}",
                case_dir.display()
            )
        })?;
    }
}

pub(crate) fn case_name_from_wrapper(
    test_group_dir: &Path,
    wrapper: &TestBuildWrapper,
    case_dir: &Path,
) -> anyhow::Result<String> {
    if case_dir == wrapper.dir {
        relative_case_name(test_group_dir, case_dir)
    } else {
        relative_case_name(&wrapper.dir, case_dir)
    }
}

fn build_config_paths(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("build-") && name.ends_with(".toml") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

pub(crate) fn discover_qemu_cases(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> anyhow::Result<Vec<DiscoveredQemuCase>> {
    if let Some(case_name) = selected_case {
        validate_selected_case_name(case_name, suite_name, group_label)?;
    }

    let config_name = qemu_config_name(arch);
    let mut cases = Vec::new();
    let mut selected_case_dirs_without_config = Vec::new();
    let build_wrappers = discover_build_wrappers(test_suite_dir, target, suite_name, group_label)?;

    for build_wrapper in &build_wrappers {
        if let Some(case_name) = selected_case {
            if case_name == build_wrapper.name {
                let qemu_config_path = build_wrapper.dir.join(&config_name);
                if qemu_config_path.is_file() {
                    cases.push(discovered_qemu_root_case(build_wrapper, qemu_config_path));
                } else if dir_contains_qemu_config(&build_wrapper.dir)? {
                    selected_case_dirs_without_config
                        .push((build_wrapper.name.clone(), qemu_config_path));
                }
                continue;
            }

            let case_dir = build_wrapper.dir.join(case_name);
            if case_dir.is_dir() {
                let qemu_config_path = case_dir.join(&config_name);
                if qemu_config_path.is_file() {
                    cases.push(discovered_qemu_case(
                        build_wrapper,
                        case_name.to_string(),
                        case_dir,
                        qemu_config_path,
                    ));
                } else {
                    selected_case_dirs_without_config
                        .push((build_wrapper.name.clone(), qemu_config_path));
                }
            }
            continue;
        }

        let root_qemu_config_path = build_wrapper.dir.join(&config_name);
        if root_qemu_config_path.is_file() {
            cases.push(discovered_qemu_root_case(
                build_wrapper,
                root_qemu_config_path,
            ));
        }

        discover_qemu_cases_in_build_wrapper(build_wrapper, &config_name, &mut cases)?;
    }

    cases.sort_by(|left, right| left.display_name.cmp(&right.display_name));

    if cases.is_empty() {
        if let Some(case_name) = selected_case {
            if !selected_case_dirs_without_config.is_empty() {
                let searched = selected_case_dirs_without_config
                    .iter()
                    .map(|(build_group, path)| format!("{build_group}: {}", path.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "{suite_name} {group_label} test case `{case_name}` exists under matching \
                     build group(s), but none provide `{config_name}` for arch `{arch}`: \
                     {searched}"
                );
            }
            bail!(
                "unknown {suite_name} {group_label} test case `{case_name}` for arch `{arch}` \
                 under {}; cases are discovered from <build_group>/<case> directories with \
                 matching `{config_name}`",
                test_suite_dir.display()
            );
        }
        bail!(
            "no {suite_name} {group_label} qemu test cases for arch `{arch}` found under {}",
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

fn discover_qemu_cases_in_build_wrapper(
    build_wrapper: &TestBuildWrapper,
    config_name: &str,
    cases: &mut Vec<DiscoveredQemuCase>,
) -> anyhow::Result<()> {
    let mut stack = fs::read_dir(&build_wrapper.dir)
        .with_context(|| format!("failed to read {}", build_wrapper.dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }

        if resolve_build_config_path(
            &case_dir,
            build_target_from_config_path(&build_wrapper.build_config_path)?,
        )?
        .is_some()
        {
            continue;
        }

        let qemu_config_path = case_dir.join(config_name);
        if qemu_config_path.is_file() {
            let case_name = relative_case_name(&build_wrapper.dir, &case_dir)?;
            cases.push(discovered_qemu_case(
                build_wrapper,
                case_name,
                case_dir,
                qemu_config_path,
            ));
            continue;
        }

        if is_case_asset_dir(&case_dir) {
            continue;
        }

        stack.extend(
            fs::read_dir(&case_dir)
                .with_context(|| format!("failed to read {}", case_dir.display()))?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }

    Ok(())
}

fn relative_case_name(root: &Path, case_dir: &Path) -> anyhow::Result<String> {
    let relative = case_dir.strip_prefix(root).with_context(|| {
        format!(
            "failed to derive case name for {} relative to {}",
            case_dir.display(),
            root.display()
        )
    })?;
    Ok(relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn is_case_asset_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("c" | "sh" | "python" | "rust")
    )
}

fn build_target_from_config_path(path: &Path) -> anyhow::Result<&str> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("invalid build config filename `{}`", path.display()))?;
    file_name
        .strip_prefix("build-")
        .and_then(|name| name.strip_suffix(".toml"))
        .with_context(|| format!("invalid build config filename `{}`", path.display()))
}

fn validate_selected_case_name(
    case_name: &str,
    suite_name: &str,
    group_label: &str,
) -> anyhow::Result<()> {
    let path = Path::new(case_name);
    let valid = !case_name.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if valid {
        return Ok(());
    }

    bail!(
        "invalid {suite_name} {group_label} test case `{case_name}`; expected a relative case \
         name without path traversal"
    )
}

fn discovered_qemu_case(
    build_wrapper: &TestBuildWrapper,
    name: String,
    case_dir: PathBuf,
    qemu_config_path: PathBuf,
) -> DiscoveredQemuCase {
    DiscoveredQemuCase {
        display_name: format!("{}/{}", build_wrapper.name, name),
        name,
        case_dir,
        qemu_config_path,
        build_group: build_wrapper.name.clone(),
        build_config_path: build_wrapper.build_config_path.clone(),
    }
}

fn discovered_qemu_root_case(
    build_wrapper: &TestBuildWrapper,
    qemu_config_path: PathBuf,
) -> DiscoveredQemuCase {
    DiscoveredQemuCase {
        name: build_wrapper.name.clone(),
        display_name: build_wrapper.name.clone(),
        case_dir: build_wrapper.dir.clone(),
        qemu_config_path,
        build_group: build_wrapper.name.clone(),
        build_config_path: build_wrapper.build_config_path.clone(),
    }
}

pub(crate) fn group_cases_by_build_config<T: BuildConfigRef>(
    cases: &[T],
) -> Vec<QemuCaseGroup<'_, T>> {
    let mut groups: Vec<QemuCaseGroup<'_, T>> = Vec::new();
    for case in cases {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.build_config_path == case.build_config_path())
        {
            group.cases.push(case);
        } else {
            groups.push(QemuCaseGroup {
                build_group: case.build_group(),
                build_config_path: case.build_config_path(),
                cases: vec![case],
            });
        }
    }

    groups
}

pub(crate) fn normalize_qemu_test_commands<I, S>(
    qemu_config_path: &Path,
    commands: I,
    suite_name: &str,
) -> anyhow::Result<Vec<String>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut test_commands = Vec::new();
    for command in commands {
        let command = command.as_ref().trim().to_string();
        if command.is_empty() {
            bail!(
                "{suite_name} grouped qemu case `{}` contains an empty test command",
                qemu_config_path.display()
            );
        }
        test_commands.push(command);
    }
    Ok(test_commands)
}

pub(crate) fn validate_grouped_qemu_commands(
    qemu: &QemuConfig,
    case: &TestQemuCase,
    suite_name: &str,
) -> anyhow::Result<()> {
    let shell_init_cmd_set = qemu
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if shell_init_cmd_set && !case.test_commands.is_empty() {
        bail!(
            "{suite_name} grouped qemu case `{}` cannot define both `shell_init_cmd` and \
             `test_commands`",
            case.qemu_config_path.display()
        );
    }
    Ok(())
}

pub(crate) fn apply_smp_qemu_arg(qemu: &mut QemuConfig, smp: Option<usize>) {
    let Some(cpu_num) = smp else {
        return;
    };

    if let Some(index) = qemu.args.iter().position(|arg| arg == "-smp")
        && let Some(value) = qemu.args.get_mut(index + 1)
    {
        *value = cpu_num.to_string();
        return;
    }

    qemu.args.push("-smp".to_string());
    qemu.args.push(cpu_num.to_string());
}

pub(crate) fn smp_from_qemu_arg(qemu: &QemuConfig) -> Option<usize> {
    let index = qemu.args.iter().position(|arg| arg == "-smp")?;
    let value = qemu.args.get(index + 1)?;
    parse_smp_qemu_value(value)
}

fn parse_smp_qemu_value(value: &str) -> Option<usize> {
    let first = value.split(',').next()?;
    if let Ok(cpu_num) = first.parse() {
        return Some(cpu_num);
    }

    value.split(',').find_map(|part| {
        let cpu_num = part.strip_prefix("cpus=")?;
        cpu_num.parse().ok()
    })
}

pub(crate) fn apply_timeout_scale(qemu: &mut QemuConfig) {
    let Some(timeout) = qemu.timeout else {
        return;
    };
    if timeout == 0 {
        return;
    }

    let scale = match std::env::var(TIMEOUT_SCALE_ENV) {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(scale) if scale > 1 => scale,
            Ok(_) | Err(_) => {
                eprintln!(
                    "warning: ignoring invalid {TIMEOUT_SCALE_ENV} value `{}`; expected integer > \
                     1",
                    value.trim()
                );
                return;
            }
        },
        Err(_) => return,
    };

    qemu.timeout = timeout.checked_mul(scale).or(Some(u64::MAX));
}

pub(crate) fn qemu_timeout_summary(qemu: &QemuConfig) -> String {
    match qemu.timeout {
        Some(0) | None => "disabled".to_string(),
        Some(timeout) => format!("{timeout}s"),
    }
}

pub(crate) fn parse_test_target(
    arch: &Option<String>,
    target: &Option<String>,
    suite_name: &str,
    supported_arches: &[&str],
    supported_targets: &[&str],
    resolve_arch_and_target: impl FnOnce(
        Option<String>,
        Option<String>,
    ) -> anyhow::Result<(String, String)>,
) -> anyhow::Result<(String, String)> {
    let (arch, target) = resolve_arch_and_target(arch.clone(), target.clone())?;
    validate_supported_target(&arch, suite_name, "arch values", supported_arches)?;
    validate_supported_target(&target, suite_name, "targets", supported_targets)?;
    Ok((arch, target))
}

pub(crate) fn finalize_qemu_test_run(
    suite_name: &str,
    unit: &str,
    failed: &[String],
) -> anyhow::Result<()> {
    if failed.is_empty() {
        println!("all {} qemu tests passed", suite_name);
        Ok(())
    } else {
        bail!(
            "{} qemu tests failed for {} {}(s): {}",
            suite_name,
            failed.len(),
            unit,
            failed.join(", ")
        )
    }
}

pub(crate) fn render_case_tree<I, S>(group: &str, cases: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    #[derive(Default)]
    struct Node {
        children: BTreeMap<String, Node>,
    }

    fn render_node(node: &Node, prefix: &str, lines: &mut Vec<String>) {
        let total = node.children.len();
        for (index, (name, child)) in node.children.iter().enumerate() {
            let is_last = index + 1 == total;
            let branch = if is_last { "└── " } else { "├── " };
            lines.push(format!("{prefix}{branch}{name}"));

            let child_prefix = if is_last { "    " } else { "│   " };
            render_node(child, &format!("{prefix}{child_prefix}"), lines);
        }
    }

    let mut root = Node::default();
    for case in cases {
        let mut node = &mut root;
        for part in case.as_ref().split('/').filter(|part| !part.is_empty()) {
            node = node.children.entry(part.to_string()).or_default();
        }
    }

    let mut lines = vec![group.to_string()];
    render_node(&root, "", &mut lines);
    lines.join("\n")
}

pub(crate) fn unsupported_uboot_test_command(os: &str) -> anyhow::Result<()> {
    bail!(
        "{os} does not support `test uboot` yet; only axvisor currently implements a U-Boot test \
         suite"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_failure_summary_is_aggregated() {
        let err = finalize_qemu_test_run(
            "arceos",
            "package",
            &["pkg-b".to_string(), "pkg-c".to_string()],
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("arceos qemu tests failed for 2 package(s): pkg-b, pkg-c")
        );
    }

    #[test]
    fn unsupported_uboot_error_is_explicit() {
        let err = unsupported_uboot_test_command("arceos").unwrap_err();

        assert!(
            err.to_string()
                .contains("arceos does not support `test uboot` yet")
        );
    }

    #[test]
    fn render_case_tree_uses_group_root() {
        assert_eq!(
            render_case_tree(
                "normal",
                [
                    "qemu-smp1/apk-curl",
                    "qemu-smp1/smoke",
                    "qemu-smp4/affinity",
                ],
            ),
            "normal\n├── qemu-smp1\n│   ├── apk-curl\n│   └── smoke\n└── qemu-smp4\n    └── \
             affinity"
        );
    }

    #[test]
    fn discover_all_qemu_cases_includes_wrapper_root_case() {
        let root = tempfile::tempdir().unwrap();
        let case_dir = root.path().join("suite/root-case");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join("build-x86_64-unknown-none.toml"), "").unwrap();
        fs::write(case_dir.join("qemu-x86_64.toml"), "").unwrap();

        let cases =
            discover_all_qemu_cases(&root.path().join("suite"), None, "test", "qemu").unwrap();

        assert_eq!(cases, ["root-case"]);
    }

    #[test]
    fn discover_qemu_cases_includes_wrapper_root_case() {
        let root = tempfile::tempdir().unwrap();
        let case_dir = root.path().join("suite/root-case");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join("build-x86_64-unknown-none.toml"), "").unwrap();
        let qemu_config = case_dir.join("qemu-x86_64.toml");
        fs::write(&qemu_config, "").unwrap();

        let cases = discover_qemu_cases(
            &root.path().join("suite"),
            "x86_64",
            "x86_64-unknown-none",
            None,
            "test",
            "qemu",
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].name, "root-case");
        assert_eq!(cases[0].display_name, "root-case");
        assert_eq!(cases[0].case_dir, case_dir);
        assert_eq!(cases[0].qemu_config_path, qemu_config);
    }

    #[test]
    fn resolve_build_config_accepts_target_specific_name_only() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("build-x86_64-unknown-none.toml");
        fs::write(&path, "target = \"x86_64-unknown-none\"\n").unwrap();

        assert_eq!(
            resolve_build_config_path(root.path(), "x86_64-unknown-none").unwrap(),
            Some(path)
        );
    }

    #[test]
    fn resolve_build_config_rejects_legacy_names() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("build-x86_64.toml"), "").unwrap();

        let err = resolve_build_config_path(root.path(), "x86_64-unknown-none")
            .unwrap_err()
            .to_string();

        assert!(err.contains("unsupported legacy build config name"));
        assert!(err.contains("build-x86_64-unknown-none.toml"));
    }

    #[test]
    fn selected_qemu_case_rejects_path_traversal() {
        let root = tempfile::tempdir().unwrap();
        let build_dir = root.path().join("suite/wrapper");
        fs::create_dir_all(&build_dir).unwrap();
        let build_config = build_dir.join("build-x86_64-unknown-none.toml");
        fs::write(&build_config, "").unwrap();

        let err = discover_qemu_cases(
            root.path().join("suite").as_path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("../escape"),
            "test",
            "qemu",
        )
        .unwrap_err()
        .to_string();

        assert!(err.contains("invalid test qemu test case"));
        assert!(err.contains("path traversal"));
    }
}
