use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use ostool::board::config::BoardRunConfig;
use serde::Deserialize;

#[derive(Default, Deserialize)]
struct AppMetadata {
    board_shell_prefix: Option<String>,
}

use super::{
    StarryAppBoardCase,
    build_config::{
        collect_prefixed_toml_files, default_build_config_for_board_config,
        discover_case_build_config,
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
    let (build_config_path, target) =
        match default_build_config_for_board_config(workspace_root, &board_config_path)? {
            Some((board_build_config, target)) => (board_build_config, target),
            None => discover_case_build_config(&case_dir, None)?,
        };
    let board_shell_prefix = read_app_metadata(&case_dir)?.board_shell_prefix;

    Ok(StarryAppBoardCase {
        name: case_name.to_string(),
        case_dir,
        init_path,
        init_cmd,
        build_config_path,
        board_config_path,
        target,
        board_shell_prefix,
    })
}

pub(crate) fn configure_board_init_step(
    board: &mut BoardRunConfig,
    init_cmd: &str,
    board_shell_prefix: Option<&str>,
) -> anyhow::Result<()> {
    match board.shell_check_steps.as_mut_slice() {
        [step] => {
            if let Some(metadata_prefix) = board_shell_prefix {
                if let Some(step_prefix) = step.shell_prefix.as_deref() {
                    ensure!(
                        step_prefix == metadata_prefix,
                        "Starry app board metadata `board_shell_prefix` conflicts with the shell \
                         check step prefix"
                    );
                } else {
                    step.shell_prefix = Some(metadata_prefix.to_string());
                }
            }
            ensure!(
                step.shell_prefix.is_some(),
                "Starry app board shell check step requires `shell_prefix` or app metadata \
                 `board_shell_prefix`"
            );
            step.shell_cmd = Some(merge_board_init_command(
                init_cmd,
                step.shell_cmd.as_deref(),
            ));
        }
        [] => {
            let _ = (board_shell_prefix, init_cmd);
            bail!("Starry app board config must define `shell_check_steps` before board init");
        }
        _ => bail!("Starry app board config must define at most one shell check step"),
    }
    Ok(())
}

fn merge_board_init_command(init_cmd: &str, board_prelude: Option<&str>) -> String {
    match board_prelude
        .map(str::trim)
        .filter(|prelude| !prelude.is_empty())
    {
        Some(prelude) => format!("{prelude}\n{init_cmd}"),
        None => init_cmd.to_string(),
    }
}

fn read_app_metadata(case_dir: &Path) -> anyhow::Result<AppMetadata> {
    let path = case_dir.join("app.toml");
    if !path.is_file() {
        return Ok(AppMetadata::default());
    }
    toml::from_str(
        &fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?,
    )
    .with_context(|| format!("failed to parse {}", path.display()))
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
