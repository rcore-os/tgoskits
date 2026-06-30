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
    collect_apps_in_dir(&apps_dir, &apps_dir, &ignored, &mut apps)?;
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
}

fn collect_apps_in_dir(
    apps_dir: &Path,
    dir: &Path,
    ignored: &BTreeSet<String>,
    apps: &mut Vec<StarryAppCase>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("failed to read {}", dir.display()))? {
        let entry = entry?;
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }
        let name = relative_app_name(apps_dir, &case_dir)?;
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
            });
            continue;
        }
        collect_apps_in_dir(apps_dir, &case_dir, ignored, apps)?;
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
        (true, true, _) => bail!(
            "Starry app `{}` has both qemu-* and board-* configs; split it or make kind explicit",
            case_dir.display()
        ),
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

pub(super) fn available_case_names(apps_dir: &Path) -> anyhow::Result<String> {
    let mut cases = Vec::new();
    for entry in
        fs::read_dir(apps_dir).with_context(|| format!("failed to read {}", apps_dir.display()))?
    {
        let entry = entry?;
        if !entry.path().is_dir() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        cases.push(name);
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
