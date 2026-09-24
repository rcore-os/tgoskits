use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};

use super::{
    build_config::collect_prefixed_toml_files,
    types::{StarryAppCase, StarryAppKind},
};

/// Case-name prefix used for cases that live under `apps/benchmark/starry`.
/// It keeps nightly-only benchmarks distinct from the equally named QEMU smoke
/// cases that stay in `apps/starry`.
pub(super) const BENCHMARK_APP_PREFIX: &str = "benchmark";

pub(crate) fn discover_apps(workspace_root: &Path) -> anyhow::Result<Vec<StarryAppCase>> {
    discover_apps_with_ignore(workspace_root, true)
}

pub(super) fn discover_apps_with_ignore(
    workspace_root: &Path,
    respect_ignore: bool,
) -> anyhow::Result<Vec<StarryAppCase>> {
    let apps_dir = apps_starry_dir(workspace_root);
    ensure!(
        apps_dir.is_dir(),
        "missing Starry apps directory `{}`",
        apps_dir.display()
    );

    let ignored = if respect_ignore {
        ignored_app_names(workspace_root)?
    } else {
        BTreeSet::new()
    };
    let mut apps = Vec::new();
    collect_apps_in_dir(&apps_dir, &apps_dir, "", false, &ignored, &mut apps)?;
    let benchmark_dir = apps_benchmark_starry_dir(workspace_root);
    if benchmark_dir.is_dir() {
        collect_apps_in_dir(
            &benchmark_dir,
            &benchmark_dir,
            BENCHMARK_APP_PREFIX,
            true,
            &ignored,
            &mut apps,
        )?;
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
}

fn collect_apps_in_dir(
    apps_dir: &Path,
    dir: &Path,
    name_prefix: &str,
    benchmark: bool,
    ignored: &BTreeSet<String>,
    apps: &mut Vec<StarryAppCase>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }
        let relative = relative_app_name(apps_dir, &case_dir)?;
        let name = if name_prefix.is_empty() {
            relative
        } else {
            format!("{name_prefix}/{relative}")
        };
        if is_ignored_app(ignored, &name) {
            continue;
        }
        if let Some(kind) = infer_app_kind(&case_dir)? {
            apps.push(StarryAppCase {
                name,
                kind,
                prebuild_path: optional_file(case_dir.join("prebuild.sh")),
                requires: read_requires(&case_dir)?,
                case_dir,
                benchmark,
            });
            continue;
        }
        collect_apps_in_dir(apps_dir, &case_dir, name_prefix, benchmark, ignored, apps)?;
    }
    Ok(())
}

fn relative_app_name(apps_dir: &Path, case_dir: &Path) -> anyhow::Result<String> {
    let relative = case_dir.strip_prefix(apps_dir).with_context(|| {
        format!(
            "failed to make {} relative to {}",
            case_dir.display(),
            apps_dir.display()
        )
    })?;
    Ok(relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/"))
}

fn optional_file(path: PathBuf) -> Option<PathBuf> {
    path.is_file().then_some(path)
}

fn ignored_app_names(workspace_root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let path = workspace_root.join("apps/.ignore");
    if !path.is_file() {
        return Ok(BTreeSet::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.trim_matches('/').to_string())
        .collect())
}

fn is_ignored_app(ignored: &BTreeSet<String>, name: &str) -> bool {
    ignored.contains(name)
        || ignored.contains(&format!("starry/{name}"))
        || ignored.contains(&format!("apps/starry/{name}"))
        || match name
            .strip_prefix(BENCHMARK_APP_PREFIX)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            Some(relative) => {
                ignored.contains(&format!("benchmark/{relative}"))
                    || ignored.contains(&format!("apps/benchmark/starry/{relative}"))
            }
            None => false,
        }
}

fn infer_app_kind(case_dir: &Path) -> anyhow::Result<Option<StarryAppKind>> {
    let has_qemu = !collect_prefixed_toml_files(case_dir, "qemu-")?.is_empty();
    let has_board = case_dir.join("init.sh").is_file()
        && !collect_prefixed_toml_files(case_dir, "board-")?.is_empty();
    let has_prebuild = case_dir.join("prebuild.sh").is_file();

    match (has_qemu, has_board, has_prebuild) {
        (true, false, _) => Ok(Some(StarryAppKind::Qemu)),
        (false, true, _) => Ok(Some(StarryAppKind::Board)),
        (false, false, true) => Ok(Some(StarryAppKind::Qemu)),
        (false, false, false) => Ok(None),
        (true, true, _) => Ok(Some(StarryAppKind::Both)),
    }
}

fn read_requires(case_dir: &Path) -> anyhow::Result<Vec<String>> {
    let path = case_dir.join("requires");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect())
}

pub(super) fn apps_starry_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("apps/starry")
}

pub(super) fn apps_benchmark_starry_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("apps/benchmark/starry")
}

/// Resolve a selected case name (optionally prefixed with `benchmark/`) to the
/// case directory that owns it, accepting both the functional `apps/starry`
/// tree and the nightly `apps/benchmark/starry` tree.
pub(super) fn resolve_case_dir(workspace_root: &Path, case_name: &str) -> anyhow::Result<PathBuf> {
    let case_name = validate_case_name(case_name)?;
    let (apps_dir, relative) = case_root_and_relative(workspace_root, case_name);
    ensure!(
        apps_dir.is_dir(),
        "missing Starry apps directory `{}`",
        apps_dir.display()
    );
    let case_dir = apps_dir.join(relative);
    if !case_dir.is_dir() {
        bail!(
            "unknown Starry app case `{case_name}` in {}; available cases: {}",
            apps_dir.display(),
            available_case_names(workspace_root)?
        );
    }
    Ok(case_dir)
}

fn case_root_and_relative(workspace_root: &Path, case_name: &str) -> (PathBuf, PathBuf) {
    match case_name
        .strip_prefix(BENCHMARK_APP_PREFIX)
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|relative| !relative.is_empty())
    {
        Some(relative) => (
            apps_benchmark_starry_dir(workspace_root),
            PathBuf::from(relative),
        ),
        None => (apps_starry_dir(workspace_root), PathBuf::from(case_name)),
    }
}

pub(super) fn validate_case_name(case_name: &str) -> anyhow::Result<&str> {
    let case_name = case_name.trim();
    ensure!(!case_name.is_empty(), "Starry app case name is empty");
    let path = Path::new(case_name);
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_))),
        "invalid Starry app case name `{case_name}`"
    );
    Ok(case_name)
}

pub(super) fn available_case_names(workspace_root: &Path) -> anyhow::Result<String> {
    let mut cases = Vec::new();
    for (apps_dir, prefix) in [
        (apps_starry_dir(workspace_root), ""),
        (
            apps_benchmark_starry_dir(workspace_root),
            BENCHMARK_APP_PREFIX,
        ),
    ] {
        if !apps_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&apps_dir)
            .with_context(|| format!("failed to read {}", apps_dir.display()))?
        {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if prefix.is_empty() {
                cases.push(name);
            } else {
                cases.push(format!("{prefix}/{name}"));
            }
        }
    }
    cases.sort();
    if cases.is_empty() {
        Ok("<none>".to_string())
    } else {
        Ok(cases.join(", "))
    }
}

pub(super) fn resolve_case_relative_path(case_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    let case_relative = case_dir.join(path);
    if case_relative.exists() {
        case_relative
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
#[path = "tests/discovery.rs"]
mod tests;
