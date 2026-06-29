use super::{types::list_qemu_cases_unexpected_error, *};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WalkQemuCaseDir {
    Descend,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct IndexedQemuCase {
    name: String,
    display_name: String,
    case_dir: PathBuf,
    qemu_configs: BTreeMap<String, PathBuf>,
    variant: Option<String>,
    build_group: String,
    build_config_path: PathBuf,
}

pub(crate) fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

pub(super) fn legacy_build_config_candidates(dir: &Path, target: &str) -> Vec<PathBuf> {
    let Some(arch) = arch_from_target_name(target) else {
        return Vec::new();
    };
    [dir.join(format!("build-{arch}.toml"))]
        .into_iter()
        .filter(|path| path.is_file())
        .collect()
}

pub(super) fn arch_from_target_name(target: &str) -> Option<&str> {
    target.split_once('-').map(|(arch, _)| arch)
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

    let cases = discover_qemu_case_index(test_group_dir, None, None)
        .map_err(list_qemu_cases_unexpected_error)?
        .into_iter()
        .filter(|case| indexed_case_matches_selected(case, selected_case))
        .map(|case| case.name)
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

pub(super) fn discover_all_qemu_cases_with_metadata(
    test_group_dir: &Path,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> ListQemuCasesResult<Vec<(String, Vec<String>)>> {
    if let Some(case_name) = selected_case {
        validate_selected_case_name(case_name, suite_name, group_label)
            .map_err(list_qemu_cases_unexpected_error)?;
    }

    let cases = discover_qemu_case_index(test_group_dir, None, None)
        .map_err(list_qemu_cases_unexpected_error)?
        .into_iter()
        .filter(|case| indexed_case_matches_selected(case, selected_case))
        .map(|case| {
            (
                case.display_name,
                case.qemu_configs.keys().cloned().collect::<Vec<_>>(),
            )
        })
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

pub(super) fn walk_qemu_case_dirs(
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

pub(super) fn ensure_listed_qemu_cases_not_empty<T>(
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
        return Err(ListQemuCasesError::new(
            ListQemuCasesErrorKind::UnknownSelectedCase,
            format!(
                "unknown {suite_name} {group_label} test case `{case_name}` under {}; cases are \
                 discovered from build wrapper directories with qemu-*.toml",
                test_group_dir.display()
            ),
        ));
    }
    Err(ListQemuCasesError::new(
        ListQemuCasesErrorKind::EmptyGroup,
        format!(
            "no {suite_name} {group_label} qemu test cases found under {}",
            test_group_dir.display()
        ),
    ))
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
                    variant: None,
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

pub(super) fn build_config_paths(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
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

pub(super) fn resolve_build_config_paths(
    dir: &Path,
    target: &str,
) -> anyhow::Result<Vec<(Option<String>, PathBuf)>> {
    let mut paths = Vec::new();
    let canonical = dir.join(format!("build-{target}.toml"));
    if canonical.is_file() {
        paths.push((None, canonical));
    }

    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(variant) = build_config_variant(name, target) else {
            continue;
        };
        paths.push((Some(variant.to_string()), path));
    }

    if paths.is_empty() {
        let legacy_candidates = legacy_build_config_candidates(dir, target);
        if !legacy_candidates.is_empty() {
            bail!(
                "unsupported legacy build config name(s) under {}: {}; expected only \
                 `build-{target}.toml` or `build-{target}-<variant>.toml`",
                dir.display(),
                legacy_candidates
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    paths.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(paths)
}

pub(super) fn build_config_variant<'a>(name: &'a str, target: &str) -> Option<&'a str> {
    let suffix = name
        .strip_prefix(&format!("build-{target}-"))?
        .strip_suffix(".toml")?;
    (!suffix.is_empty()).then_some(suffix)
}

pub(crate) fn discover_qemu_cases(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> anyhow::Result<Vec<DiscoveredQemuCase>> {
    discover_qemu_cases_impl(
        test_suite_dir,
        arch,
        target,
        selected_case,
        suite_name,
        group_label,
        false,
    )
}

pub(crate) fn discover_qemu_cases_allow_empty(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
) -> anyhow::Result<Vec<DiscoveredQemuCase>> {
    discover_qemu_cases_impl(
        test_suite_dir,
        arch,
        target,
        selected_case,
        suite_name,
        group_label,
        true,
    )
}

pub(super) fn discover_qemu_cases_impl(
    test_suite_dir: &Path,
    arch: &str,
    target: &str,
    selected_case: Option<&str>,
    suite_name: &str,
    group_label: &str,
    allow_empty: bool,
) -> anyhow::Result<Vec<DiscoveredQemuCase>> {
    if let Some(case_name) = selected_case {
        validate_selected_case_name(case_name, suite_name, group_label)?;
    }

    let mut cases = Vec::new();
    let mut selected_case_dirs_without_config = Vec::new();
    for case in discover_qemu_case_index(test_suite_dir, Some(target), selected_case)? {
        if !indexed_case_matches_selected(&case, selected_case) {
            continue;
        }
        let config_key = qemu_config_key_for_wrapper(arch, case.variant.as_deref());
        if let Some(qemu_config_path) = case.qemu_configs.get(&config_key) {
            cases.push(DiscoveredQemuCase {
                name: case.name,
                display_name: case.display_name,
                case_dir: case.case_dir,
                qemu_config_path: qemu_config_path.clone(),
                build_group: case.build_group,
                build_config_path: case.build_config_path,
            });
        } else if selected_case.is_some() {
            let config_name = qemu_config_name(&config_key);
            selected_case_dirs_without_config
                .push((case.build_group, case.case_dir.join(&config_name)));
        }
    }

    if cases.is_empty() {
        if let Some(case_name) = selected_case {
            if !selected_case_dirs_without_config.is_empty() {
                let expected_configs = selected_case_dirs_without_config
                    .iter()
                    .filter_map(|(_, path)| path.file_name())
                    .map(|name| format!("`{}`", name.to_string_lossy()))
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(", ");
                let searched = selected_case_dirs_without_config
                    .iter()
                    .map(|(build_group, path)| format!("{build_group}: {}", path.display()))
                    .collect::<Vec<_>>()
                    .join(", ");
                bail!(
                    "{suite_name} {group_label} test case `{case_name}` exists under matching \
                     build group(s), but none provide {expected_configs} for arch `{arch}`: \
                     {searched}"
                );
            }

            bail!(
                "unknown {suite_name} {group_label} test case `{case_name}` for arch `{arch}` \
                 under {}; cases are discovered from <build_group>/<case> directories with \
                 matching qemu config files",
                test_suite_dir.display()
            );
        }

        if allow_empty {
            return Ok(cases);
        }

        bail!(
            "no {suite_name} {group_label} qemu test cases for arch `{arch}` found under {}",
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

pub(super) fn discover_qemu_case_index(
    test_group_dir: &Path,
    target: Option<&str>,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<IndexedQemuCase>> {
    let mut cases = Vec::new();
    let mut build_wrappers = Vec::new();
    let mut stack = fs::read_dir(test_group_dir)
        .with_context(|| format!("failed to read {}", test_group_dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;

    while let Some(entry) = stack.pop() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }

        let build_config_path = match target {
            Some(target) => {
                let build_config_paths = resolve_build_config_paths(&dir, target)?;
                if !build_config_paths.is_empty() {
                    for (variant, build_config_path) in build_config_paths {
                        build_wrappers.push(TestBuildWrapper {
                            name: build_wrapper_name(test_group_dir, &dir, variant.as_deref())?,
                            dir: dir.clone(),
                            build_config_path,
                            variant,
                        });
                    }
                    continue;
                }
                None
            }
            None => build_config_paths(&dir)?
                .into_iter()
                .next()
                .map(|path| (None, path)),
        };

        if let Some((variant, build_config_path)) = build_config_path {
            build_wrappers.push(TestBuildWrapper {
                name: build_wrapper_name(test_group_dir, &dir, variant.as_deref())?,
                dir,
                build_config_path,
                variant,
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

    build_wrappers.sort_by(|left, right| left.name.cmp(&right.name));
    for build_wrapper in &build_wrappers {
        collect_qemu_cases_in_build_wrapper(build_wrapper, selected_case, &mut cases)?;
    }
    cases.sort_by(|left, right| left.display_name.cmp(&right.display_name));
    Ok(cases)
}

pub(super) fn collect_qemu_cases_in_build_wrapper(
    build_wrapper: &TestBuildWrapper,
    selected_case: Option<&str>,
    cases: &mut Vec<IndexedQemuCase>,
) -> anyhow::Result<()> {
    if let Some(qemu_configs) = qemu_configs_in_dir(&build_wrapper.dir)?
        && !qemu_configs.is_empty()
    {
        cases.push(indexed_qemu_root_case(build_wrapper, qemu_configs));
    }

    walk_qemu_case_dirs(
        &build_wrapper.dir,
        |case_dir| {
            if case_dir == build_wrapper.dir {
                return Ok(WalkQemuCaseDir::Descend);
            }
            if !build_config_paths(case_dir)?.is_empty() {
                return Ok(WalkQemuCaseDir::Skip);
            }
            if let Some(qemu_configs) = qemu_configs_in_dir(case_dir)?
                && !qemu_configs.is_empty()
            {
                cases.push(indexed_qemu_case(
                    build_wrapper,
                    relative_case_name(&build_wrapper.dir, case_dir)?,
                    case_dir.to_path_buf(),
                    qemu_configs,
                ));
                return Ok(WalkQemuCaseDir::Skip);
            }
            if let Some(selected_case) = selected_case {
                let case_name = relative_case_name(&build_wrapper.dir, case_dir)?;
                if case_name == selected_case {
                    cases.push(indexed_qemu_case(
                        build_wrapper,
                        case_name,
                        case_dir.to_path_buf(),
                        BTreeMap::new(),
                    ));
                    return Ok(WalkQemuCaseDir::Skip);
                }
            }
            Ok(WalkQemuCaseDir::Descend)
        },
        |case_dir| format!("failed to read qemu case directory {}", case_dir.display()),
    )
}

pub(super) fn qemu_configs_in_dir(dir: &Path) -> anyhow::Result<Option<BTreeMap<String, PathBuf>>> {
    let mut configs = BTreeMap::new();
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
            configs.insert(arch.to_string(), path);
        }
    }
    Ok((!configs.is_empty()).then_some(configs))
}

pub(super) fn qemu_config_key_for_wrapper(arch: &str, variant: Option<&str>) -> String {
    match variant {
        Some(variant) => format!("{arch}-{variant}"),
        None => arch.to_string(),
    }
}

pub(super) fn indexed_case_matches_selected(
    case: &IndexedQemuCase,
    selected_case: Option<&str>,
) -> bool {
    let Some(selected_case) = selected_case else {
        return true;
    };
    case.name == selected_case
        || case.name.starts_with(&format!("{selected_case}/"))
        || case.display_name == selected_case
        || case.display_name.starts_with(&format!("{selected_case}/"))
}

pub(super) fn indexed_qemu_case(
    build_wrapper: &TestBuildWrapper,
    name: String,
    case_dir: PathBuf,
    qemu_configs: BTreeMap<String, PathBuf>,
) -> IndexedQemuCase {
    let name = case_name_for_wrapper_variant(name, build_wrapper.variant.as_deref());
    IndexedQemuCase {
        display_name: format!("{}/{}", build_wrapper.name, name),
        name,
        case_dir,
        qemu_configs,
        variant: build_wrapper.variant.clone(),
        build_group: build_wrapper.name.clone(),
        build_config_path: build_wrapper.build_config_path.clone(),
    }
}

pub(super) fn indexed_qemu_root_case(
    build_wrapper: &TestBuildWrapper,
    qemu_configs: BTreeMap<String, PathBuf>,
) -> IndexedQemuCase {
    IndexedQemuCase {
        name: build_wrapper.name.clone(),
        display_name: build_wrapper.name.clone(),
        case_dir: build_wrapper.dir.clone(),
        qemu_configs,
        variant: build_wrapper.variant.clone(),
        build_group: build_wrapper.name.clone(),
        build_config_path: build_wrapper.build_config_path.clone(),
    }
}

pub(super) fn build_wrapper_name(
    test_group_dir: &Path,
    dir: &Path,
    variant: Option<&str>,
) -> anyhow::Result<String> {
    let name = relative_case_name(test_group_dir, dir)?;
    Ok(case_name_for_wrapper_variant(name, variant))
}

pub(super) fn case_name_for_wrapper_variant(name: String, variant: Option<&str>) -> String {
    match variant {
        Some(variant) => format!("{name}-{variant}"),
        None => name,
    }
}

pub(super) fn relative_case_name(root: &Path, case_dir: &Path) -> anyhow::Result<String> {
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

pub(super) fn is_case_asset_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("c" | "sh" | "python" | "rust")
    )
}

pub(super) fn validate_selected_case_name(
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
