use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, anyhow, bail};
use serde::Deserialize;

use crate::{
    context::resolve_axvisor_arch_and_target,
    test::{case::TestQemuCase, qemu::parse_test_target},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AxvisorQemuCase {
    pub(crate) case: TestQemuCase,
    pub(crate) build_config: Option<PathBuf>,
    pub(crate) vmconfigs: Vec<PathBuf>,
}

const TEST_ARCHES: &[&str] = &["aarch64", "riscv64", "x86_64", "loongarch64"];
const TEST_TARGETS: &[&str] = &[
    "aarch64-unknown-none-softfloat",
    "riscv64gc-unknown-none-elf",
    "x86_64-unknown-none",
    "loongarch64-unknown-none-softfloat",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UbootBoardConfig {
    pub(crate) board: &'static str,
    pub(crate) guest: &'static str,
    pub(crate) build_config: &'static str,
    pub(crate) vmconfig: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) build_config: PathBuf,
    pub(crate) vmconfigs: Vec<PathBuf>,
    pub(crate) board_test_config_path: PathBuf,
}

const UBOOT_BOARD_CONFIGS: &[UbootBoardConfig] = &[
    UbootBoardConfig {
        board: "orangepi-5-plus",
        guest: "linux",
        build_config: "os/axvisor/configs/board/orangepi-5-plus.toml",
        vmconfig: "os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml",
    },
    UbootBoardConfig {
        board: "phytiumpi",
        guest: "linux",
        build_config: "os/axvisor/configs/board/phytiumpi.toml",
        vmconfig: "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
    },
    UbootBoardConfig {
        board: "roc-rk3568-pc",
        guest: "linux",
        build_config: "os/axvisor/configs/board/roc-rk3568-pc.toml",
        vmconfig: "os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml",
    },
];

#[derive(Debug, Deserialize)]
struct AxvisorQemuCaseConfig {
    build_config: Option<PathBuf>,
    shell_init_cmd: Option<String>,
    #[serde(default)]
    test_commands: Vec<String>,
    #[serde(default)]
    vmconfigs: Vec<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct AxvisorBoardCaseConfig {
    build_config: PathBuf,
    vmconfigs: Vec<PathBuf>,
}

pub(crate) fn parse_target(
    arch: &Option<String>,
    target: &Option<String>,
) -> anyhow::Result<(String, String)> {
    parse_test_target(
        arch,
        target,
        "axvisor qemu tests",
        TEST_ARCHES,
        TEST_TARGETS,
        resolve_axvisor_arch_and_target,
    )
}

pub(crate) fn discover_qemu_cases(
    workspace_root: &Path,
    group: &str,
    arch: &str,
    selected_case: Option<&str>,
) -> anyhow::Result<Vec<AxvisorQemuCase>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    let config_name = qemu_config_name(arch);

    if let Some(case_name) = selected_case {
        let case_dir = test_suite_dir.join(case_name);
        if !case_dir.is_dir() {
            bail!(
                "unknown Axvisor qemu test case `{case_name}` in {}; available cases are \
                 discovered from direct subdirectories",
                test_suite_dir.display()
            );
        }

        let qemu_config_path = case_dir.join(&config_name);
        if !qemu_config_path.is_file() {
            bail!(
                "Axvisor test case `{case_name}` does not provide `{}`",
                qemu_config_path.display()
            );
        }

        return Ok(vec![load_qemu_case(
            case_name.to_string(),
            case_dir,
            qemu_config_path,
        )?]);
    }

    let mut cases = Vec::new();
    for entry in fs::read_dir(&test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let qemu_config_path = path.join(&config_name);
        if qemu_config_path.is_file() {
            cases.push(load_qemu_case(name, path, qemu_config_path)?);
        }
    }
    cases.sort_by(|left, right| left.case.name.cmp(&right.case.name));

    if cases.is_empty() {
        bail!(
            "no Axvisor qemu test cases for arch `{arch}` found under {}",
            test_suite_dir.display()
        );
    }

    Ok(cases)
}

fn load_qemu_case(
    name: String,
    case_dir: PathBuf,
    qemu_config_path: PathBuf,
) -> anyhow::Result<AxvisorQemuCase> {
    let config = load_qemu_case_config(&qemu_config_path)?;
    let test_commands = qemu_case_test_commands(&qemu_config_path, &config)?;

    Ok(AxvisorQemuCase {
        case: TestQemuCase {
            name,
            case_dir,
            qemu_config_path,
            test_commands,
            subcases: Vec::new(),
        },
        build_config: config.build_config,
        vmconfigs: config.vmconfigs,
    })
}

fn load_qemu_case_config(qemu_config_path: &Path) -> anyhow::Result<AxvisorQemuCaseConfig> {
    let content = fs::read_to_string(qemu_config_path)
        .with_context(|| format!("failed to read {}", qemu_config_path.display()))?;
    toml::from_str(&content)
        .with_context(|| format!("failed to parse {}", qemu_config_path.display()))
}

fn qemu_case_test_commands(
    qemu_config_path: &Path,
    config: &AxvisorQemuCaseConfig,
) -> anyhow::Result<Vec<String>> {
    let shell_init_cmd = config
        .shell_init_cmd
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let mut test_commands = Vec::with_capacity(config.test_commands.len());
    for command in &config.test_commands {
        let command = command.trim().to_string();
        if command.is_empty() {
            bail!(
                "Axvisor grouped qemu case `{}` contains an empty test command",
                qemu_config_path.display()
            );
        }
        test_commands.push(command);
    }

    if shell_init_cmd.is_some() && !test_commands.is_empty() {
        bail!(
            "Axvisor grouped qemu case `{}` cannot define both `shell_init_cmd` and \
             `test_commands`",
            qemu_config_path.display()
        );
    }

    Ok(test_commands)
}

pub(crate) fn uboot_board_config(board: &str, guest: &str) -> anyhow::Result<UbootBoardConfig> {
    UBOOT_BOARD_CONFIGS
        .iter()
        .copied()
        .find(|config| config.board == board && config.guest == guest)
        .ok_or_else(|| {
            anyhow!(
                "unsupported axvisor uboot test target board=`{}` guest=`{}`. Supported \
                 board/guest pairs are: {}",
                board,
                guest,
                supported_board_guest_pairs()
            )
        })
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
    board: Option<&str>,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    let mut groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    groups.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.board_name.cmp(&right.board_name))
    });

    if let Some(name) = selected_case {
        let available = groups
            .iter()
            .map(|group| group.name.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        groups.retain(|group| group.name == name);
        if groups.is_empty() {
            return Err(anyhow!(
                "unsupported axvisor board test case `{}`. Supported cases are: {}",
                name,
                if available.is_empty() {
                    "<none>".to_string()
                } else {
                    available
                }
            ));
        }
    }

    if let Some(board) = board {
        let available = groups
            .iter()
            .map(|group| group.board_name.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join(", ");
        groups.retain(|group| group.board_name == board);
        if groups.is_empty() {
            return Err(anyhow!(
                "unsupported axvisor board test board `{}`. Supported boards are: {}",
                board,
                if available.is_empty() {
                    "<none>".to_string()
                } else {
                    available
                }
            ));
        }
    }

    if groups.is_empty() {
        bail!(
            "no Axvisor board test groups found under {}",
            test_suite_dir.display()
        );
    }

    Ok(groups)
}

fn supported_board_guest_pairs() -> String {
    UBOOT_BOARD_CONFIGS
        .iter()
        .map(|config| format!("{}/{}", config.board, config.guest))
        .collect::<Vec<_>>()
        .join(", ")
}

fn collect_board_test_groups(
    workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let mut groups = Vec::new();
    for entry in fs::read_dir(test_suite_dir)
        .with_context(|| format!("failed to read {}", test_suite_dir.display()))?
    {
        let entry = entry?;
        let case_dir = entry.path();
        if !case_dir.is_dir() {
            continue;
        }

        let case_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        for config_entry in fs::read_dir(&case_dir)
            .with_context(|| format!("failed to read {}", case_dir.display()))?
        {
            let config_entry = config_entry?;
            let config_path = config_entry.path();
            if !config_path.is_file() || config_path.extension().is_none_or(|ext| ext != "toml") {
                continue;
            }

            let Some(stem) = config_path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some(board_case_name) = stem.strip_prefix("board-") else {
                continue;
            };

            let config = load_board_case_config(&config_path)?;
            let build_config = resolve_workspace_path(workspace_root, config.build_config);
            ensure_file_exists(
                &build_config,
                &format!("Axvisor board test group `{case_name}/{board_case_name}` build_config"),
            )?;

            if config.vmconfigs.is_empty() {
                bail!("Axvisor board test group `{case_name}/{board_case_name}` has no vmconfigs");
            }
            let vmconfigs = config
                .vmconfigs
                .into_iter()
                .map(|path| resolve_workspace_path(workspace_root, path))
                .collect::<Vec<_>>();
            for vmconfig in &vmconfigs {
                ensure_file_exists(
                    vmconfig,
                    &format!("Axvisor board test group `{case_name}/{board_case_name}` vmconfig"),
                )?;
            }

            groups.push(BoardTestGroup {
                name: case_name.clone(),
                board_name: board_case_name.to_string(),
                build_config,
                vmconfigs,
                board_test_config_path: config_path,
            });
        }
    }

    Ok(groups)
}

fn load_board_case_config(path: &Path) -> anyhow::Result<AxvisorBoardCaseConfig> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))
}

fn resolve_workspace_path(workspace_root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

fn ensure_file_exists(path: &Path, label: &str) -> anyhow::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!("{label} maps to missing file `{}`", path.display())
    }
}

fn test_suite_dir(workspace_root: &Path, group: &str) -> anyhow::Result<PathBuf> {
    let test_suite_root = workspace_root.join("test-suit/axvisor");
    let test_suite_dir = test_suite_root.join(group);
    if test_suite_dir.is_dir() {
        Ok(test_suite_dir)
    } else {
        bail!(
            "unsupported Axvisor test group `{group}`. Supported groups are: {}",
            supported_test_groups(&test_suite_root)?
        )
    }
}

fn supported_test_groups(test_suite_root: &Path) -> anyhow::Result<String> {
    let mut groups = Vec::new();
    if test_suite_root.is_dir() {
        for entry in fs::read_dir(test_suite_root)
            .with_context(|| format!("failed to read {}", test_suite_root.display()))?
        {
            let entry = entry?;
            if entry.path().is_dir()
                && let Ok(name) = entry.file_name().into_string()
            {
                groups.push(name);
            }
        }
    }
    groups.sort();
    Ok(if groups.is_empty() {
        "<none>".to_string()
    } else {
        groups.join(", ")
    })
}

fn qemu_config_name(arch: &str) -> String {
    format!("qemu-{arch}.toml")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn write_qemu_config(root: &Path, case: &str, arch: &str, body: &str) -> PathBuf {
        write_qemu_config_in_group(root, "normal", case, arch, body)
    }

    fn write_qemu_config_in_group(
        root: &Path,
        group: &str,
        case: &str,
        arch: &str,
        body: &str,
    ) -> PathBuf {
        let dir = root.join("test-suit/axvisor").join(group).join(case);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("qemu-{arch}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    fn write_board_config(root: &Path, case: &str, name: &str, body: &str) -> PathBuf {
        write_board_config_in_group(root, "normal", case, name, body)
    }

    fn write_board_config_in_group(
        root: &Path,
        group: &str,
        case: &str,
        name: &str,
        body: &str,
    ) -> PathBuf {
        let dir = root.join("test-suit/axvisor").join(group).join(case);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("board-{name}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    fn write_file(root: &Path, path: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    #[test]
    fn parses_supported_arch_aliases() {
        assert_eq!(
            parse_target(&Some("aarch64".to_string()), &None).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&Some("x86_64".to_string()), &None).unwrap(),
            ("x86_64".to_string(), "x86_64-unknown-none".to_string())
        );
        assert_eq!(
            parse_target(&Some("loongarch64".to_string()), &None).unwrap(),
            (
                "loongarch64".to_string(),
                "loongarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&Some("riscv64".to_string()), &None).unwrap(),
            (
                "riscv64".to_string(),
                "riscv64gc-unknown-none-elf".to_string()
            )
        );
    }

    #[test]
    fn accepts_full_target_triples() {
        assert_eq!(
            parse_target(&None, &Some("aarch64-unknown-none-softfloat".to_string())).unwrap(),
            (
                "aarch64".to_string(),
                "aarch64-unknown-none-softfloat".to_string()
            )
        );
        assert_eq!(
            parse_target(&None, &Some("riscv64gc-unknown-none-elf".to_string())).unwrap(),
            (
                "riscv64".to_string(),
                "riscv64gc-unknown-none-elf".to_string()
            )
        );
        assert_eq!(
            parse_target(
                &None,
                &Some("loongarch64-unknown-none-softfloat".to_string())
            )
            .unwrap(),
            (
                "loongarch64".to_string(),
                "loongarch64-unknown-none-softfloat".to_string()
            )
        );
    }

    #[test]
    fn rejects_unsupported_arches() {
        let err = parse_target(&Some("mips64".to_string()), &None).unwrap_err();
        let err = err.to_string();

        assert!(err.contains("mips64"));
        assert!(err.contains("aarch64"));
        assert!(err.contains("loongarch64"));
        assert!(err.contains("riscv64"));
        assert!(err.contains("x86_64"));
    }

    #[test]
    fn discovers_only_cases_with_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "build_config = \"os/axvisor/configs/board/qemu-aarch64.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-qemu-smp1.toml\"]\nshell_prefix = \"~ \
             #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = []\n",
        );
        write_qemu_config(
            root.path(),
            "x86-only",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );

        let cases = discover_qemu_cases(root.path(), "normal", "aarch64", None).unwrap();

        assert_eq!(
            cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["smoke"]
        );
        assert_eq!(
            cases[0].vmconfigs,
            vec![PathBuf::from(
                "os/axvisor/configs/vms/linux-aarch64-qemu-smp1.toml"
            )]
        );
        assert_eq!(
            cases[0].build_config,
            Some(PathBuf::from("os/axvisor/configs/board/qemu-aarch64.toml"))
        );
    }

    #[test]
    fn selected_case_requires_matching_qemu_config() {
        let root = tempdir().unwrap();
        write_qemu_config(
            root.path(),
            "smoke",
            "x86_64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"hello_world\"\nsuccess_regex = \
             []\nfail_regex = []\n",
        );

        let err = discover_qemu_cases(root.path(), "normal", "aarch64", Some("smoke")).unwrap_err();

        assert!(err.to_string().contains("does not provide `"));
    }

    #[test]
    fn discovers_qemu_cases_from_selected_group() {
        let root = tempdir().unwrap();
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );
        write_qemu_config_in_group(
            root.path(),
            "stress",
            "load",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"stress\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        let cases = discover_qemu_cases(root.path(), "stress", "aarch64", None).unwrap();

        assert_eq!(
            cases
                .iter()
                .map(|case| case.case.name.as_str())
                .collect::<Vec<_>>(),
            vec!["load"]
        );
    }

    #[test]
    fn rejects_unknown_qemu_test_group() {
        let root = tempdir().unwrap();
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \">>\"\nshell_init_cmd = \"normal\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        let err = discover_qemu_cases(root.path(), "unknown", "aarch64", None).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported Axvisor test group `unknown`")
        );
        assert!(err.to_string().contains("normal"));
    }

    #[test]
    fn parses_uboot_board_config_for_linux_smoke() {
        assert_eq!(
            uboot_board_config("orangepi-5-plus", "linux").unwrap(),
            UbootBoardConfig {
                board: "orangepi-5-plus",
                guest: "linux",
                build_config: "os/axvisor/configs/board/orangepi-5-plus.toml",
                vmconfig: "os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml",
            }
        );
        assert_eq!(
            uboot_board_config("phytiumpi", "linux").unwrap(),
            UbootBoardConfig {
                board: "phytiumpi",
                guest: "linux",
                build_config: "os/axvisor/configs/board/phytiumpi.toml",
                vmconfig: "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
            }
        );
        assert_eq!(
            uboot_board_config("roc-rk3568-pc", "linux").unwrap(),
            UbootBoardConfig {
                board: "roc-rk3568-pc",
                guest: "linux",
                build_config: "os/axvisor/configs/board/roc-rk3568-pc.toml",
                vmconfig: "os/axvisor/configs/vms/linux-aarch64-rk3568-smp1.toml",
            }
        );
    }

    #[test]
    fn rejects_unsupported_uboot_board() {
        let err = uboot_board_config("unknown-board", "linux").unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported axvisor uboot test target board=`unknown-board`")
        );
        assert!(err.to_string().contains("orangepi-5-plus/linux"));
        assert!(err.to_string().contains("phytiumpi/linux"));
        assert!(err.to_string().contains("roc-rk3568-pc/linux"));
    }

    #[test]
    fn returns_all_board_test_groups_when_no_filter_is_given() {
        let root = tempdir().unwrap();
        write_file(root.path(), "os/axvisor/configs/board/phytiumpi.toml");
        write_file(root.path(), "os/axvisor/configs/board/orangepi-5-plus.toml");
        write_file(
            root.path(),
            "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
        );
        write_file(
            root.path(),
            "os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml",
        );
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "build_config = \"os/axvisor/configs/board/phytiumpi.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml\"]\nboard_type = \
             \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "build_config = \"os/axvisor/configs/board/orangepi-5-plus.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml\"]\nboard_type = \
             \"OrangePi-5-Plus\"\n",
        );

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/orangepi-5-plus-linux", "smoke/phytiumpi-linux"]
        );
    }

    #[test]
    fn filters_board_test_group_by_case() {
        let root = tempdir().unwrap();
        write_file(root.path(), "os/axvisor/configs/board/phytiumpi.toml");
        write_file(
            root.path(),
            "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
        );
        let board_test_config = write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "build_config = \"os/axvisor/configs/board/phytiumpi.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml\"]\nboard_type = \
             \"PhytiumPi\"\n",
        );

        let groups =
            discover_board_test_groups(root.path(), "normal", Some("smoke"), None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "phytiumpi-linux");
        assert_eq!(
            groups[0].vmconfigs,
            vec![
                root.path()
                    .join("os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml")
            ]
        );
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_groups_by_board() {
        let root = tempdir().unwrap();
        write_file(root.path(), "os/axvisor/configs/board/phytiumpi.toml");
        write_file(root.path(), "os/axvisor/configs/board/orangepi-5-plus.toml");
        write_file(
            root.path(),
            "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
        );
        write_file(
            root.path(),
            "os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml",
        );
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "build_config = \"os/axvisor/configs/board/phytiumpi.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml\"]\nboard_type = \
             \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "syscall",
            "phytiumpi-linux",
            "build_config = \"os/axvisor/configs/board/phytiumpi.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml\"]\nboard_type = \
             \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "build_config = \"os/axvisor/configs/board/orangepi-5-plus.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-orangepi5p-smp1.toml\"]\nboard_type = \
             \"OrangePi-5-Plus\"\n",
        );

        let groups =
            discover_board_test_groups(root.path(), "normal", None, Some("phytiumpi-linux"))
                .unwrap();

        assert_eq!(
            groups
                .iter()
                .map(|group| format!("{}/{}", group.name, group.board_name))
                .collect::<Vec<_>>(),
            vec!["smoke/phytiumpi-linux", "syscall/phytiumpi-linux"]
        );
    }

    #[test]
    fn rejects_unknown_board_test_board() {
        let root = tempdir().unwrap();
        write_file(root.path(), "os/axvisor/configs/board/phytiumpi.toml");
        write_file(
            root.path(),
            "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
        );
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "build_config = \"os/axvisor/configs/board/phytiumpi.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml\"]\nboard_type = \
             \"PhytiumPi\"\n",
        );

        let err =
            discover_board_test_groups(root.path(), "normal", None, Some("unknown")).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported axvisor board test board `unknown`")
        );
        assert!(err.to_string().contains("phytiumpi-linux"));
    }

    #[test]
    fn rejects_unknown_board_test_case() {
        let root = tempdir().unwrap();
        write_file(root.path(), "os/axvisor/configs/board/phytiumpi.toml");
        write_file(
            root.path(),
            "os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml",
        );
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "build_config = \"os/axvisor/configs/board/phytiumpi.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml\"]\nboard_type = \
             \"PhytiumPi\"\n",
        );

        let err =
            discover_board_test_groups(root.path(), "normal", Some("unknown"), None).unwrap_err();

        assert!(
            err.to_string()
                .contains("unsupported axvisor board test case `unknown`")
        );
        assert!(err.to_string().contains("smoke"));
    }

    #[test]
    fn board_case_config_is_also_valid_board_run_config() {
        let config: ostool::board::config::BoardRunConfig = toml::from_str(
            "build_config = \"os/axvisor/configs/board/phytiumpi.toml\"\nvmconfigs = \
             [\"os/axvisor/configs/vms/linux-aarch64-e2000-smp1.toml\"]\nboard_type = \
             \"PhytiumPi\"\nshell_prefix = \"login:\"\nshell_init_cmd = \"root\"\nsuccess_regex = \
             [\"(?m)^root@.*#\\\\s*$\"]\n",
        )
        .unwrap();

        assert_eq!(config.board_type, "PhytiumPi");
        assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
    }
}
