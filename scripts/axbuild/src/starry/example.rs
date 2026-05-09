use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail, ensure};
use clap::{Args, Subcommand};
use serde::Deserialize;

use super::board;

#[derive(Args, Debug, Clone)]
pub struct ArgsExample {
    #[command(subcommand)]
    pub command: ExampleCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ExampleCommand {
    /// Build and run a StarryOS example on a remote board
    Board(ArgsExampleBoard),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsExampleBoard {
    /// Select examples/starry/<CASE>
    #[arg(short = 't', long = "test-case", value_name = "CASE")]
    pub test_case: String,

    #[arg(long = "board-config")]
    pub board_config: Option<PathBuf>,

    #[arg(short = 'b', long)]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StarryExampleBoardCase {
    pub(crate) name: String,
    pub(crate) case_dir: PathBuf,
    pub(crate) init_path: PathBuf,
    pub(crate) init_cmd: String,
    pub(crate) build_config_path: PathBuf,
    pub(crate) board_config_path: PathBuf,
    pub(crate) target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildConfigCandidate {
    path: PathBuf,
    target: String,
}

#[derive(Debug, Deserialize)]
struct BuildConfigTarget {
    target: Option<String>,
}

pub(crate) fn resolve_board_case(
    workspace_root: &Path,
    case_name: &str,
    explicit_board_config: Option<&Path>,
) -> anyhow::Result<StarryExampleBoardCase> {
    let case_name = validate_case_name(case_name)?;
    let examples_dir = examples_starry_dir(workspace_root);
    ensure!(
        examples_dir.is_dir(),
        "missing Starry examples directory `{}`",
        examples_dir.display()
    );

    let case_dir = examples_dir.join(case_name);
    if !case_dir.is_dir() {
        bail!(
            "unknown Starry example case `{case_name}` in {}; available cases: {}",
            examples_dir.display(),
            available_case_names(&examples_dir)?
        );
    }

    let init_path = case_dir.join("init.sh");
    ensure!(
        init_path.is_file(),
        "Starry example case `{case_name}` is missing `{}`",
        init_path.display()
    );
    let init_cmd = fs::read_to_string(&init_path)
        .with_context(|| format!("failed to read {}", init_path.display()))?;
    let init_cmd = init_cmd.trim().to_string();
    ensure!(
        !init_cmd.is_empty(),
        "Starry example case `{case_name}` has an empty init script `{}`",
        init_path.display()
    );

    let board_config_path = match explicit_board_config {
        Some(path) => resolve_explicit_board_config(&case_dir, path),
        None => discover_case_board_config(&case_dir)?,
    };
    let default_target = default_target_for_board_config(workspace_root, &board_config_path)?;
    let (build_config_path, target) =
        discover_case_build_config(&case_dir, default_target.as_deref())?;

    Ok(StarryExampleBoardCase {
        name: case_name.to_string(),
        case_dir,
        init_path,
        init_cmd,
        build_config_path,
        board_config_path,
        target,
    })
}

fn examples_starry_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("examples/starry")
}

fn validate_case_name(case_name: &str) -> anyhow::Result<&str> {
    let case_name = case_name.trim();
    ensure!(!case_name.is_empty(), "Starry example case name is empty");
    let path = Path::new(case_name);
    ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
            && path.components().count() == 1,
        "invalid Starry example case name `{case_name}`"
    );
    Ok(case_name)
}

fn available_case_names(examples_dir: &Path) -> anyhow::Result<String> {
    let mut cases = Vec::new();
    for entry in fs::read_dir(examples_dir)
        .with_context(|| format!("failed to read {}", examples_dir.display()))?
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

fn discover_case_board_config(case_dir: &Path) -> anyhow::Result<PathBuf> {
    let mut configs = collect_prefixed_toml_files(case_dir, "board-")?;
    match configs.len() {
        0 => bail!(
            "Starry example case `{}` does not provide a board-<board>.toml config",
            case_dir.display()
        ),
        1 => Ok(configs.remove(0)),
        _ => bail!(
            "Starry example case `{}` provides multiple board configs; pass --board-config",
            case_dir.display()
        ),
    }
}

fn resolve_explicit_board_config(case_dir: &Path, path: &Path) -> PathBuf {
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

fn discover_case_build_config(
    case_dir: &Path,
    preferred_target: Option<&str>,
) -> anyhow::Result<(PathBuf, String)> {
    let mut candidates = collect_build_config_candidates(case_dir)?;
    ensure!(
        !candidates.is_empty(),
        "Starry example case `{}` does not provide a build-<target>.toml config",
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
            "Starry example case `{}` provides multiple build configs; pass a board config that \
             maps to one target or keep one build config",
            case_dir.display()
        ),
    }
}

fn collect_prefixed_toml_files(case_dir: &Path, prefix: &str) -> anyhow::Result<Vec<PathBuf>> {
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

fn default_target_for_board_config(
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
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn write_case_file(root: &Path, case_name: &str, name: &str, body: &str) -> PathBuf {
        let path = root.join("examples/starry").join(case_name).join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    fn write_board_default(root: &Path, board_name: &str, target: &str) -> PathBuf {
        let path = root
            .join("os/StarryOS/configs/board")
            .join(format!("{board_name}.toml"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "target = \"{target}\"\nenv = {{}}\nfeatures = []\nlog = \"Info\"\nplat_dyn = \
                 true\n"
            ),
        )
        .unwrap();
        path
    }

    fn write_minimal_case(root: &Path, case_name: &str) {
        write_case_file(root, case_name, "init.sh", "echo hello\n");
        write_case_file(
            root,
            case_name,
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root,
            case_name,
            "build-aarch64-unknown-none-softfloat.toml",
            "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = []\nlog = \
             \"Info\"\nplat_dyn = true\n",
        );
    }

    #[test]
    fn resolves_board_case_from_examples_dir() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");

        let case = resolve_board_case(root.path(), "demo", None).unwrap();

        assert_eq!(case.name, "demo");
        assert_eq!(case.target, "aarch64-unknown-none-softfloat");
        assert_eq!(case.init_cmd, "echo hello");
        assert!(
            case.board_config_path
                .ends_with("board-orangepi-5-plus.toml")
        );
        assert!(
            case.build_config_path
                .ends_with("build-aarch64-unknown-none-softfloat.toml")
        );
    }

    #[test]
    fn reports_missing_examples_dir() {
        let root = tempdir().unwrap();

        let err = resolve_board_case(root.path(), "demo", None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("missing Starry examples directory"));
        assert!(err.contains("examples/starry"));
    }

    #[test]
    fn reports_unknown_case_with_available_cases() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");

        let err = resolve_board_case(root.path(), "missing", None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("unknown Starry example case `missing`"));
        assert!(err.contains("demo"));
    }

    #[test]
    fn reads_build_target_from_filename_when_toml_target_is_absent() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
        write_case_file(
            root.path(),
            "demo",
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-aarch64-unknown-none-softfloat.toml",
            "env = {}\nfeatures = []\nlog = \"Info\"\nplat_dyn = true\n",
        );

        let case = resolve_board_case(root.path(), "demo", None).unwrap();

        assert_eq!(case.target, "aarch64-unknown-none-softfloat");
    }

    #[test]
    fn rejects_mismatched_build_target_filename() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
        write_case_file(
            root.path(),
            "demo",
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-aarch64-unknown-none-softfloat.toml",
            "target = \"x86_64-unknown-none\"\nenv = {}\nfeatures = []\nlog = \"Info\"\nplat_dyn \
             = false\n",
        );

        let err = resolve_board_case(root.path(), "demo", None)
            .unwrap_err()
            .to_string();

        assert!(err.contains("does not match filename target"));
    }

    #[test]
    fn explicit_board_config_overrides_case_config() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");
        let explicit = root.path().join("custom-board.toml");
        fs::write(&explicit, "board_type = \"custom\"\n").unwrap();

        let case = resolve_board_case(root.path(), "demo", Some(explicit.as_path())).unwrap();

        assert_eq!(case.board_config_path, explicit);
    }

    #[test]
    fn explicit_relative_board_config_can_resolve_inside_case() {
        let root = tempdir().unwrap();
        write_minimal_case(root.path(), "demo");
        let explicit = write_case_file(
            root.path(),
            "demo",
            "board-custom.toml",
            "board_type = \"Custom\"\nshell_prefix = \"root@starry:/root #\"\n",
        );

        let case =
            resolve_board_case(root.path(), "demo", Some(Path::new("board-custom.toml"))).unwrap();

        assert_eq!(case.board_config_path, explicit);
    }

    #[test]
    fn board_default_target_picks_matching_build_config() {
        let root = tempdir().unwrap();
        write_case_file(root.path(), "demo", "init.sh", "echo hello\n");
        write_case_file(
            root.path(),
            "demo",
            "board-orangepi-5-plus.toml",
            "board_type = \"OrangePi-5-Plus\"\nshell_prefix = \"root@starry:/root #\"\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-aarch64-unknown-none-softfloat.toml",
            "target = \"aarch64-unknown-none-softfloat\"\nenv = {}\nfeatures = []\nlog = \
             \"Info\"\nplat_dyn = true\n",
        );
        write_case_file(
            root.path(),
            "demo",
            "build-riscv64gc-unknown-none-elf.toml",
            "target = \"riscv64gc-unknown-none-elf\"\nenv = {}\nfeatures = []\nlog = \
             \"Info\"\nplat_dyn = false\n",
        );
        write_board_default(
            root.path(),
            "orangepi-5-plus",
            "aarch64-unknown-none-softfloat",
        );

        let case = resolve_board_case(root.path(), "demo", None).unwrap();

        assert_eq!(case.target, "aarch64-unknown-none-softfloat");
    }
}
