use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};

use super::{
    StarryAppBoardCase,
    build_config::{
        collect_prefixed_toml_files, default_target_for_board_config, discover_case_build_config,
    },
    discovery::{
        apps_starry_dir, available_case_names, resolve_case_relative_path, validate_case_name,
    },
};

pub(crate) fn resolve_board_case(
    workspace_root: &Path,
    case_name: &str,
    explicit_board_config: Option<&Path>,
) -> anyhow::Result<StarryAppBoardCase> {
    let case_name = validate_case_name(case_name)?;
    let apps_dir = apps_starry_dir(workspace_root);
    ensure!(
        apps_dir.is_dir(),
        "missing Starry apps directory `{}`",
        apps_dir.display()
    );

    let case_dir = apps_dir.join(case_name);
    if !case_dir.is_dir() {
        bail!(
            "unknown Starry app case `{case_name}` in {}; available cases: {}",
            apps_dir.display(),
            available_case_names(&apps_dir)?
        );
    }

    let init_path = case_dir.join("init.sh");
    ensure!(
        init_path.is_file(),
        "Starry app case `{case_name}` is missing `{}`",
        init_path.display()
    );
    let init_cmd = fs::read_to_string(&init_path)
        .with_context(|| format!("failed to read {}", init_path.display()))?;
    let init_cmd = init_cmd.trim().to_string();
    ensure!(
        !init_cmd.is_empty(),
        "Starry app case `{case_name}` has an empty init script `{}`",
        init_path.display()
    );

    let board_config_path = match explicit_board_config {
        Some(path) => resolve_explicit_board_config(&case_dir, path),
        None => discover_case_board_config(&case_dir)?,
    };
    let default_target = default_target_for_board_config(workspace_root, &board_config_path)?;
    let (build_config_path, target) =
        discover_case_build_config(&case_dir, default_target.as_deref())?;

    Ok(StarryAppBoardCase {
        name: case_name.to_string(),
        case_dir,
        init_path,
        init_cmd,
        build_config_path,
        board_config_path,
        target,
    })
}

fn discover_case_board_config(case_dir: &Path) -> anyhow::Result<PathBuf> {
    let mut configs = collect_prefixed_toml_files(case_dir, "board-")?;
    match configs.len() {
        0 => bail!(
            "Starry app case `{}` does not provide a board-<board>.toml config",
            case_dir.display()
        ),
        1 => Ok(configs.remove(0)),
        _ => bail!(
            "Starry app case `{}` provides multiple board configs; pass --board-config",
            case_dir.display()
        ),
    }
}

fn resolve_explicit_board_config(case_dir: &Path, path: &Path) -> PathBuf {
    resolve_case_relative_path(case_dir, path)
}

#[cfg(test)]
#[path = "tests/board.rs"]
mod tests;
