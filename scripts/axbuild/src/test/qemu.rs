use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListedQemuCase {
    pub(crate) name: String,
    pub(crate) archs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListQemuCasesErrorKind {
    EmptyGroup,
    UnknownSelectedCase,
    Unexpected,
}

#[derive(Debug)]
pub(crate) struct ListQemuCasesError {
    kind: ListQemuCasesErrorKind,
    message: String,
}

impl ListQemuCasesError {
    pub(crate) fn kind(&self) -> ListQemuCasesErrorKind {
        self.kind
    }
}

impl std::fmt::Display for ListQemuCasesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for ListQemuCasesError {}

impl From<anyhow::Error> for ListQemuCasesError {
    fn from(err: anyhow::Error) -> Self {
        list_qemu_cases_unexpected_error(err)
    }
}

pub(crate) type ListQemuCasesResult<T> = Result<T, ListQemuCasesError>;

pub(crate) struct QemuCaseGroup<'a, T> {
    pub(crate) build_group: &'a str,
    pub(crate) build_config_path: &'a Path,
    pub(crate) cases: Vec<&'a T>,
}

pub(crate) struct QemuCaseBuildGroup<'a, T, R> {
    pub(crate) group: QemuCaseGroup<'a, T>,
    pub(crate) request: R,
    pub(crate) cargo: Cargo,
}

pub(crate) trait BuildConfigRef {
    fn build_group(&self) -> &str;
    fn build_config_path(&self) -> &Path;
}

type ListQemuRootCollector<T> = fn(&TestBuildWrapper) -> anyhow::Result<Option<T>>;
type ListQemuChildCollector<T> =
    fn(&TestBuildWrapper, &Path) -> anyhow::Result<Option<(String, T)>>;

struct ListedQemuCaseCollector<T> {
    cases: BTreeMap<String, T>,
    collect_root: ListQemuRootCollector<T>,
    collect_child: ListQemuChildCollector<T>,
}

impl<T> ListedQemuCaseCollector<T> {
    fn collect_build_wrapper(
        &mut self,
        build_wrapper: &TestBuildWrapper,
        selected_case: Option<&str>,
    ) -> anyhow::Result<()> {
        if selected_case.is_none_or(|case_name| case_name == build_wrapper.name)
            && let Some(value) = (self.collect_root)(build_wrapper)?
        {
            self.cases.insert(build_wrapper.name.clone(), value);
        }

        walk_qemu_case_dirs(
            &build_wrapper.dir,
            |case_dir| {
                if !case_dir_matches_selected(&build_wrapper.dir, case_dir, selected_case)? {
                    return Ok(WalkQemuCaseDir::Skip);
                }
                if !build_config_paths(case_dir)?.is_empty() {
                    return Ok(WalkQemuCaseDir::Skip);
                }
                if let Some((name, value)) = (self.collect_child)(build_wrapper, case_dir)? {
                    self.cases.insert(name, value);
                    return Ok(WalkQemuCaseDir::Skip);
                }
                Ok(WalkQemuCaseDir::Descend)
            },
            |case_dir| format!("failed to read qemu case directory {}", case_dir.display()),
        )
    }

    fn finish(self) -> Vec<(String, T)> {
        self.cases.into_iter().collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WalkQemuCaseDir {
    Descend,
    Skip,
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
) -> ListQemuCasesResult<Vec<String>> {
    if let Some(case_name) = selected_case {
        validate_selected_case_name(case_name, suite_name, group_label)
            .map_err(list_qemu_cases_unexpected_error)?;
    }

    let by_name = discover_all_qemu_cases_with_collector(
        test_group_dir,
        selected_case,
        collect_qemu_case_root_marker,
        collect_qemu_case_child_marker,
    )
    .map_err(list_qemu_cases_unexpected_error)?;

    let cases = by_name
        .into_iter()
        .map(|(name, ())| name)
        .collect::<Vec<_>>();
    ensure_listed_qemu_cases_not_empty(
        &cases,
        selected_case,
        suite_name,
        group_label,
        test_group_dir,
    )?;
    Ok(cases)
}

pub(crate) fn discover_all_qemu_cases_with_archs(
    test_group_dir: &Path,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> ListQemuCasesResult<Vec<ListedQemuCase>> {
    discover_all_qemu_cases_with_metadata(test_group_dir, selected_case, suite_name, group_label)
        .map(|cases| {
            cases
                .into_iter()
                .map(|(name, archs)| ListedQemuCase { name, archs })
                .collect()
        })
}

fn discover_all_qemu_cases_with_metadata(
    test_group_dir: &Path,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> ListQemuCasesResult<Vec<(String, Vec<String>)>> {
    if let Some(case_name) = selected_case {
        validate_selected_case_name(case_name, suite_name, group_label)
            .map_err(list_qemu_cases_unexpected_error)?;
    }

    let cases = discover_all_qemu_cases_with_collector(
        test_group_dir,
        selected_case,
        collect_qemu_case_root_archs,
        collect_qemu_case_child_archs,
    )
    .map_err(list_qemu_cases_unexpected_error)?;
    ensure_listed_qemu_cases_not_empty(
        &cases,
        selected_case,
        suite_name,
        group_label,
        test_group_dir,
    )?;
    Ok(cases)
}

fn discover_all_qemu_cases_with_collector<T>(
    test_group_dir: &Path,
    selected_case: Option<&str>,
    collect_root: ListQemuRootCollector<T>,
    collect_child: ListQemuChildCollector<T>,
) -> anyhow::Result<Vec<(String, T)>> {
    let mut collector = ListedQemuCaseCollector {
        cases: BTreeMap::new(),
        collect_root,
        collect_child,
    };

    walk_qemu_case_dirs(
        test_group_dir,
        |dir| {
            let build_configs = build_config_paths(dir)?;
            if !build_configs.is_empty() {
                collector.collect_build_wrapper(
                    &TestBuildWrapper {
                        name: relative_case_name(test_group_dir, dir)?,
                        dir: dir.to_path_buf(),
                        build_config_path: build_configs[0].clone(),
                    },
                    selected_case,
                )?;
                return Ok(WalkQemuCaseDir::Skip);
            }
            Ok(WalkQemuCaseDir::Descend)
        },
        |dir| format!("failed to read {}", dir.display()),
    )?;

    Ok(collector.finish())
}

fn walk_qemu_case_dirs(
    root: &Path,
    mut visit: impl FnMut(&Path) -> anyhow::Result<WalkQemuCaseDir>,
    read_error: impl Fn(&Path) -> String,
) -> anyhow::Result<()> {
    let mut stack = fs::read_dir(root)
        .with_context(|| read_error(root))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        if matches!(visit(&dir)?, WalkQemuCaseDir::Skip) || is_case_asset_dir(&dir) {
            continue;
        }

        stack.extend(
            fs::read_dir(&dir)
                .with_context(|| read_error(&dir))?
                .collect::<Result<Vec<_>, _>>()?,
        );
    }

    Ok(())
}

fn ensure_listed_qemu_cases_not_empty<T>(
    cases: &[T],
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
    test_group_dir: &Path,
) -> ListQemuCasesResult<()> {
    if !cases.is_empty() {
        return Ok(());
    }
    if let Some(case_name) = selected_case {
        return Err(ListQemuCasesError {
            kind: ListQemuCasesErrorKind::UnknownSelectedCase,
            message: format!(
                "unknown {suite_name} {group_label} test case `{case_name}` under {}; cases are \
                 discovered from build wrapper directories with qemu-*.toml",
                test_group_dir.display()
            ),
        });
    }
    Err(ListQemuCasesError {
        kind: ListQemuCasesErrorKind::EmptyGroup,
        message: format!(
            "no {suite_name} {group_label} qemu test cases found under {}",
            test_group_dir.display()
        ),
    })
}

fn list_qemu_cases_unexpected_error(err: anyhow::Error) -> ListQemuCasesError {
    ListQemuCasesError {
        kind: ListQemuCasesErrorKind::Unexpected,
        message: err.to_string(),
    }
}

fn collect_qemu_case_root_marker(build_wrapper: &TestBuildWrapper) -> anyhow::Result<Option<()>> {
    dir_contains_qemu_config(&build_wrapper.dir).map(|found| found.then_some(()))
}

fn collect_qemu_case_child_marker(
    build_wrapper: &TestBuildWrapper,
    case_dir: &Path,
) -> anyhow::Result<Option<(String, ())>> {
    if !dir_contains_qemu_config(case_dir)? {
        return Ok(None);
    }
    Ok(Some((
        relative_case_name(&build_wrapper.dir, case_dir)?,
        (),
    )))
}

fn collect_qemu_case_root_archs(
    build_wrapper: &TestBuildWrapper,
) -> anyhow::Result<Option<Vec<String>>> {
    let archs = qemu_config_archs(&build_wrapper.dir)?;
    Ok((!archs.is_empty()).then_some(archs))
}

fn collect_qemu_case_child_archs(
    build_wrapper: &TestBuildWrapper,
    case_dir: &Path,
) -> anyhow::Result<Option<(String, Vec<String>)>> {
    let archs = qemu_config_archs(case_dir)?;
    if archs.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        relative_case_name(&build_wrapper.dir, case_dir)?,
        archs,
    )))
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

fn qemu_config_archs(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut archs = Vec::new();
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if let Some(arch) = stem.strip_prefix("qemu-")
            && !arch.starts_with("base-")
        {
            archs.push(arch.to_string());
        }
    }
    archs.sort();
    Ok(archs)
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
        return discover_selected_qemu_cases(
            test_suite_dir,
            arch,
            target,
            case_name,
            suite_name,
            group_label,
        );
    }

    let config_name = qemu_config_name(arch);
    let mut cases = Vec::new();
    let build_wrappers = discover_build_wrappers(test_suite_dir, target, suite_name, group_label)?;

    for build_wrapper in &build_wrappers {
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
        bail!(
            "no {suite_name} {group_label} qemu test cases for arch `{arch}` found under {}",
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

fn discover_selected_qemu_cases(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    selected_case: &str,
    suite_name: &str,
    group_label: &str,
) -> anyhow::Result<Vec<DiscoveredQemuCase>> {
    let config_name = qemu_config_name(arch);
    let mut cases = Vec::new();
    let mut selected_case_dirs_without_config = Vec::new();
    let mut stack = fs::read_dir(test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        if let Some(build_config_path) = resolve_build_config_path(&dir, target)? {
            let build_wrapper = TestBuildWrapper {
                name: relative_case_name(test_suite_dir, &dir)?,
                dir,
                build_config_path,
            };
            collect_selected_qemu_case_in_wrapper(
                &build_wrapper,
                selected_case,
                &config_name,
                &mut cases,
                &mut selected_case_dirs_without_config,
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

    cases.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    if !cases.is_empty() {
        return Ok(cases);
    }

    if !selected_case_dirs_without_config.is_empty() {
        let searched = selected_case_dirs_without_config
            .iter()
            .map(|(build_group, path)| format!("{build_group}: {}", path.display()))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "{suite_name} {group_label} test case `{selected_case}` exists under matching build \
             group(s), but none provide `{config_name}` for arch `{arch}`: {searched}"
        );
    }

    bail!(
        "unknown {suite_name} {group_label} test case `{selected_case}` for arch `{arch}` under \
         {}; cases are discovered from <build_group>/<case> directories with matching \
         `{config_name}`",
        test_suite_dir.display()
    )
}

fn collect_selected_qemu_case_in_wrapper(
    build_wrapper: &TestBuildWrapper,
    selected_case: &str,
    config_name: &str,
    cases: &mut Vec<DiscoveredQemuCase>,
    selected_case_dirs_without_config: &mut Vec<(String, PathBuf)>,
) -> anyhow::Result<()> {
    if selected_case == build_wrapper.name {
        let qemu_config_path = build_wrapper.dir.join(config_name);
        if qemu_config_path.is_file() {
            cases.push(discovered_qemu_root_case(build_wrapper, qemu_config_path));
        } else if dir_contains_qemu_config(&build_wrapper.dir)? {
            selected_case_dirs_without_config.push((build_wrapper.name.clone(), qemu_config_path));
        }
        return Ok(());
    }

    let case_dir = build_wrapper.dir.join(selected_case);
    if !case_dir.is_dir() {
        return Ok(());
    }

    let qemu_config_path = case_dir.join(config_name);
    if qemu_config_path.is_file() {
        cases.push(discovered_qemu_case(
            build_wrapper,
            selected_case.to_string(),
            case_dir,
            qemu_config_path,
        ));
    } else {
        selected_case_dirs_without_config.push((build_wrapper.name.clone(), qemu_config_path));
    }
    Ok(())
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
    let mut indexes = BTreeMap::<&Path, usize>::new();
    for case in cases {
        if let Some(index) = indexes.get(case.build_config_path()).copied() {
            groups[index].cases.push(case);
            continue;
        }

        let index = groups.len();
        indexes.insert(case.build_config_path(), index);
        groups.push(QemuCaseGroup {
            build_group: case.build_group(),
            build_config_path: case.build_config_path(),
            cases: vec![case],
        });
    }

    groups
}

pub(crate) fn prepare_case_build_groups<T, R>(
    cases: &[T],
    mut prepare_context: impl FnMut(&Path) -> anyhow::Result<(R, Cargo)>,
) -> anyhow::Result<Vec<QemuCaseBuildGroup<'_, T, R>>>
where
    T: BuildConfigRef,
{
    group_cases_by_build_config(cases)
        .into_iter()
        .map(|group| {
            let (request, cargo) = prepare_context(group.build_config_path)?;
            Ok(QemuCaseBuildGroup {
                group,
                request,
                cargo,
            })
        })
        .collect()
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

struct QemuArgs<'a> {
    args: &'a [String],
}

impl<'a> QemuArgs<'a> {
    fn new(args: &'a [String]) -> Self {
        Self { args }
    }

    fn option_value(&self, option: &str) -> Option<&str> {
        let index = self.args.iter().position(|arg| arg == option)?;
        self.args.get(index + 1).map(String::as_str)
    }
}

struct QemuArgsMut<'a> {
    args: &'a mut Vec<String>,
}

impl<'a> QemuArgsMut<'a> {
    fn new(args: &'a mut Vec<String>) -> Self {
        Self { args }
    }

    fn set_option_value(&mut self, option: &str, value: String) {
        if let Some(index) = self.args.iter().position(|arg| arg == option)
            && let Some(existing) = self.args.get_mut(index + 1)
        {
            *existing = value;
            return;
        }

        self.args.push(option.to_string());
        self.args.push(value);
    }
}

pub(crate) fn apply_smp_qemu_arg(qemu: &mut QemuConfig, smp: Option<usize>) {
    let Some(cpu_num) = smp else {
        return;
    };

    QemuArgsMut::new(&mut qemu.args).set_option_value("-smp", cpu_num.to_string());
}

pub(crate) fn smp_from_qemu_arg(qemu: &QemuConfig) -> Option<usize> {
    let args = QemuArgs::new(&qemu.args);
    let value = args.option_value("-smp")?;
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

#[derive(Default)]
struct CaseTreeNode {
    children: BTreeMap<String, CaseTreeNode>,
    labels: BTreeSet<String>,
}

fn insert_case_tree_path(node: &mut CaseTreeNode, path: &str) {
    insert_case_tree_path_with_label(node, path, None);
}

fn insert_case_tree_path_with_label(node: &mut CaseTreeNode, path: &str, label: Option<String>) {
    let mut current = node;
    for part in path.split('/').filter(|part| !part.is_empty()) {
        current = current.children.entry(part.to_string()).or_default();
    }
    if let Some(label) = label {
        current.labels.insert(label);
    }
}

fn render_case_tree_node(node: &CaseTreeNode, prefix: &str, lines: &mut Vec<String>) {
    let total = node.children.len();
    for (index, (name, child)) in node.children.iter().enumerate() {
        let is_last = index + 1 == total;
        let branch = if is_last { "└── " } else { "├── " };
        let label = if child.labels.is_empty() {
            String::new()
        } else {
            format!(
                " [{}]",
                child
                    .labels
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        lines.push(format!("{prefix}{branch}{name}{label}"));

        let child_prefix = if is_last { "    " } else { "│   " };
        render_case_tree_node(child, &format!("{prefix}{child_prefix}"), lines);
    }
}

pub(crate) fn render_case_tree<I, S>(group: &str, cases: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut root = CaseTreeNode::default();
    for case in cases {
        insert_case_tree_path(&mut root, case.as_ref());
    }

    let mut lines = vec![group.to_string()];
    render_case_tree_node(&root, "", &mut lines);
    lines.join("\n")
}

pub(crate) fn render_qemu_case_forest<I, G, C>(suite: &str, groups: I) -> String
where
    I: IntoIterator<Item = (G, C)>,
    G: AsRef<str>,
    C: IntoIterator<Item = ListedQemuCase>,
{
    let mut root = CaseTreeNode::default();
    for (group, cases) in groups {
        let group_node = root.children.entry(group.as_ref().to_string()).or_default();
        for case in cases {
            let label = if case.archs.is_empty() {
                None
            } else {
                Some(case.archs.join(", "))
            };
            insert_case_tree_path_with_label(group_node, &case.name, label);
        }
    }

    let mut lines = vec![suite.to_string()];
    render_case_tree_node(&root, "", &mut lines);
    lines.join("\n")
}

pub(crate) fn render_labeled_case_forest<I, G, C, N, L>(suite: &str, groups: I) -> String
where
    I: IntoIterator<Item = (G, C)>,
    G: AsRef<str>,
    C: IntoIterator<Item = (N, L)>,
    N: AsRef<str>,
    L: AsRef<str>,
{
    let mut root = CaseTreeNode::default();
    for (group, cases) in groups {
        let group_node = root.children.entry(group.as_ref().to_string()).or_default();
        for (case, label) in cases {
            insert_case_tree_path_with_label(
                group_node,
                case.as_ref(),
                Some(label.as_ref().to_string()),
            );
        }
    }

    let mut lines = vec![suite.to_string()];
    render_case_tree_node(&root, "", &mut lines);
    lines.join("\n")
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
    fn render_qemu_case_forest_appends_arch_labels_to_leaves() {
        assert_eq!(
            render_qemu_case_forest(
                "arceos",
                [(
                    "rust",
                    vec![
                        ListedQemuCase {
                            name: "task/yield".to_string(),
                            archs: vec!["aarch64".to_string(), "x86_64".to_string()],
                        },
                        ListedQemuCase {
                            name: "display".to_string(),
                            archs: vec!["x86_64".to_string()],
                        },
                    ],
                )],
            ),
            "arceos\n└── rust\n    ├── display [x86_64]\n    └── task\n        └── yield \
             [aarch64, x86_64]"
        );
    }

    #[test]
    fn render_labeled_case_forest_appends_board_labels_to_leaves() {
        assert_eq!(
            render_labeled_case_forest(
                "starry",
                [(
                    "normal",
                    vec![("smoke", "orangepi-5-plus"), ("smoke", "vision-five2"),],
                )],
            ),
            "starry\n└── normal\n    └── smoke [orangepi-5-plus, vision-five2]"
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
    fn discover_all_qemu_cases_rejects_unknown_selected_case() {
        let root = tempfile::tempdir().unwrap();
        let case_dir = root.path().join("suite/root-case");
        fs::create_dir_all(&case_dir).unwrap();
        fs::write(case_dir.join("build-x86_64-unknown-none.toml"), "").unwrap();
        fs::write(case_dir.join("qemu-x86_64.toml"), "").unwrap();

        let err =
            discover_all_qemu_cases(&root.path().join("suite"), Some("missing"), "test", "qemu")
                .unwrap_err()
                .to_string();

        assert!(err.contains("unknown test qemu test case `missing`"));
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

    #[test]
    fn selected_qemu_case_allows_same_name_in_later_wrapper() {
        let root = tempfile::tempdir().unwrap();
        let board_dir = root.path().join("suite/board-orangepi-5-plus/smoke");
        fs::create_dir_all(&board_dir).unwrap();
        fs::write(
            root.path()
                .join("suite/board-orangepi-5-plus/build-x86_64-unknown-none.toml"),
            "",
        )
        .unwrap();
        fs::write(board_dir.join("board-orangepi-5-plus.toml"), "").unwrap();

        let qemu_dir = root.path().join("suite/qemu-smp1/smoke");
        fs::create_dir_all(&qemu_dir).unwrap();
        fs::write(
            root.path()
                .join("suite/qemu-smp1/build-x86_64-unknown-none.toml"),
            "",
        )
        .unwrap();
        let qemu_config = qemu_dir.join("qemu-x86_64.toml");
        fs::write(&qemu_config, "").unwrap();

        let cases = discover_qemu_cases(
            root.path().join("suite").as_path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("smoke"),
            "test",
            "qemu",
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].build_group, "qemu-smp1");
        assert_eq!(cases[0].qemu_config_path, qemu_config);
    }

    #[test]
    fn selected_qemu_case_finds_wrapper_without_scanning_unrelated_broken_tree() {
        let root = tempfile::tempdir().unwrap();
        let target_dir = root.path().join("suite/qemu-smp1/smoke");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(
            root.path()
                .join("suite/qemu-smp1/build-x86_64-unknown-none.toml"),
            "",
        )
        .unwrap();
        fs::write(target_dir.join("qemu-x86_64.toml"), "").unwrap();
        fs::create_dir_all(root.path().join("suite/unrelated/broken")).unwrap();

        let cases = discover_qemu_cases(
            root.path().join("suite").as_path(),
            "x86_64",
            "x86_64-unknown-none",
            Some("smoke"),
            "test",
            "qemu",
        )
        .unwrap();

        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].build_group, "qemu-smp1");
    }
}
