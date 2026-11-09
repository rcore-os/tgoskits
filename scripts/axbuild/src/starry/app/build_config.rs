use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};

use crate::{
    build::{ConfigInput, load_config_input},
    starry::config::{StarryConfig, presets},
};

#[derive(Debug, Clone, PartialEq)]
struct BuildConfigCandidate {
    input: ConfigInput<StarryConfig>,
    target: String,
}

pub(super) fn discover_optional_build_config(
    workspace_root: &Path,
    case_dir: &Path,
    target: &str,
) -> anyhow::Result<Option<ConfigInput<StarryConfig>>> {
    let mut dir = Some(case_dir);
    while let Some(current_dir) = dir {
        if let Some(path) = resolve_exact_build_config_path(current_dir, target)? {
            return load_config_input(workspace_root, Some(&path), None).map(Some);
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
    workspace_root: &Path,
    case_dir: &Path,
    preferred_target: Option<&str>,
) -> anyhow::Result<(ConfigInput<StarryConfig>, String)> {
    let mut candidates = collect_build_config_candidates(workspace_root, case_dir)?;
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
        return Ok((candidate.input, candidate.target));
    }

    match candidates.len() {
        1 => {
            let candidate = candidates.remove(0);
            Ok((candidate.input, candidate.target))
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

fn collect_build_config_candidates(
    workspace_root: &Path,
    case_dir: &Path,
) -> anyhow::Result<Vec<BuildConfigCandidate>> {
    let mut paths = collect_prefixed_toml_files(case_dir, "build-")?;
    paths.extend(collect_prefixed_toml_files(case_dir, ".build-")?);
    paths.sort();
    paths.dedup();

    paths
        .into_iter()
        .map(|path| build_config_candidate(workspace_root, &path))
        .collect()
}

fn build_config_candidate(
    workspace_root: &Path,
    path: &Path,
) -> anyhow::Result<BuildConfigCandidate> {
    let mut input: ConfigInput<StarryConfig> = load_config_input(workspace_root, Some(path), None)?;
    let filename_target = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .and_then(build_config_target_from_stem);

    if let (Some(parsed), Some(filename)) = (
        input.config.common.target.as_deref(),
        filename_target.as_deref(),
    ) && parsed != filename
    {
        bail!(
            "build config `{}` target `{parsed}` does not match filename target `{filename}`",
            path.display()
        );
    }

    let target = input
        .config
        .common
        .target
        .clone()
        .or(filename_target)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "build config `{}` must define top-level `target` or use build-<target>.toml",
                path.display()
            )
        })?;
    input.config.common.target = Some(target.clone());
    Ok(BuildConfigCandidate { input, target })
}

fn build_config_target_from_stem(stem: &str) -> Option<String> {
    stem.strip_prefix("build-")
        .or_else(|| stem.strip_prefix(".build-"))
        .map(str::to_string)
        .filter(|target| !target.is_empty())
}

pub(super) fn default_build_config_for_board_config(
    workspace_root: &Path,
    case_dir: &Path,
    board_config_path: &Path,
) -> anyhow::Result<Option<(ConfigInput<StarryConfig>, String)>> {
    let Some(stem) = board_config_path.file_stem().and_then(|stem| stem.to_str()) else {
        return Ok(None);
    };
    let Some(board_name) = stem.strip_prefix("board-") else {
        return Ok(None);
    };
    let Some(preset) = presets::find_preset(board_name) else {
        return Ok(None);
    };
    let path = case_dir.join(format!("build-{}.toml", preset.common.board.target));
    if path.is_file() {
        let candidate = build_config_candidate(workspace_root, &path)?;
        return Ok(Some((candidate.input, candidate.target)));
    }
    let input = load_config_input(workspace_root, None, Some(preset.name))?;
    Ok(Some((input, preset.common.board.target.to_owned())))
}

#[cfg(test)]
#[path = "tests/build_config.rs"]
mod tests;
