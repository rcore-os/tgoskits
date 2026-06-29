use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use serde::Deserialize;

use super::super::board;

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildConfigCandidate {
    path: PathBuf,
    target: String,
}

#[derive(Debug, Deserialize)]
struct BuildConfigTarget {
    target: Option<String>,
}

pub(super) fn discover_optional_build_config(
    case_dir: &Path,
    target: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let mut dir = Some(case_dir);
    while let Some(current_dir) = dir {
        if let Some(path) = resolve_exact_build_config_path(current_dir, target)? {
            return Ok(Some(path));
        }
        dir = current_dir.parent();
    }
    Ok(None)
}

fn resolve_exact_build_config_path(dir: &Path, target: &str) -> anyhow::Result<Option<PathBuf>> {
    let path = dir.join(format!("build-{target}.toml"));
    if path.is_file() {
        return Ok(Some(path));
    }

    let legacy_candidates = legacy_build_config_candidates(dir, target);
    if !legacy_candidates.is_empty() {
        bail!(
            "unsupported legacy build config name(s) under {}: {}; expected only              \
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

pub(super) fn discover_case_build_config(
    case_dir: &Path,
    preferred_target: Option<&str>,
) -> anyhow::Result<(PathBuf, String)> {
    let mut candidates = collect_build_config_candidates(case_dir)?;
    ensure!(
        !candidates.is_empty(),
        "Starry app case `{}` does not provide a build-<target>.toml config",
        case_dir.display()
    );

    if let Some(preferred_target) = preferred_target
        && let Some(index) = candidates
            .iter()
            .position(|candidate| candidate.target == preferred_target)
    {
        let candidate = candidates.remove(index);
        return Ok((candidate.path, candidate.target));
    }

    match candidates.len() {
        1 => {
            let candidate = candidates.remove(0);
            Ok((candidate.path, candidate.target))
        }
        _ => bail!(
            "Starry app case `{}` provides multiple build configs; pass a board config that maps \
             to one target or keep one build config",
            case_dir.display()
        ),
    }
}

pub(super) fn collect_prefixed_toml_files(
    case_dir: &Path,
    prefix: &str,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut configs = Vec::new();
    for entry in
        fs::read_dir(case_dir).with_context(|| format!("failed to read {}", case_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "toml") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if stem.starts_with(prefix) {
            configs.push(path);
        }
    }
    configs.sort();
    Ok(configs)
}

fn collect_build_config_candidates(case_dir: &Path) -> anyhow::Result<Vec<BuildConfigCandidate>> {
    let mut paths = collect_prefixed_toml_files(case_dir, "build-")?;
    paths.extend(collect_prefixed_toml_files(case_dir, ".build-")?);
    paths.sort();
    paths.dedup();

    paths
        .into_iter()
        .map(|path| {
            let target = build_config_target(&path)?;
            Ok(BuildConfigCandidate { path, target })
        })
        .collect()
}

fn build_config_target(path: &Path) -> anyhow::Result<String> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let parsed: BuildConfigTarget =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    let filename_target = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(build_config_target_from_stem);

    if let (Some(parsed), Some(filename)) = (parsed.target.as_deref(), filename_target.as_deref())
        && parsed != filename
    {
        bail!(
            "build config `{}` target `{parsed}` does not match filename target `{filename}`",
            path.display()
        );
    }

    parsed.target.or(filename_target).ok_or_else(|| {
        anyhow::anyhow!(
            "build config `{}` must define top-level `target` or use build-<target>.toml",
            path.display()
        )
    })
}

fn build_config_target_from_stem(stem: &str) -> Option<String> {
    stem.strip_prefix("build-")
        .or_else(|| stem.strip_prefix(".build-"))
        .map(str::to_string)
        .filter(|target| !target.is_empty())
}

pub(super) fn default_target_for_board_config(
    workspace_root: &Path,
    board_config_path: &Path,
) -> anyhow::Result<Option<String>> {
    let Some(stem) = board_config_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };
    let Some(board_name) = stem.strip_prefix("board-") else {
        return Ok(None);
    };
    let build_config_path = workspace_root
        .join("os/StarryOS/configs/board")
        .join(format!("{board_name}.toml"));
    if !build_config_path.is_file() {
        return Ok(None);
    }
    Ok(Some(board::load_board_file(&build_config_path)?.target))
}

#[cfg(test)]
#[path = "tests/build_config.rs"]
mod tests;
