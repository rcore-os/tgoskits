use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use ostool::{board::RunBoardOptions, run::uboot::UbootConfig};

use super::{ArgsTest, ArgsTestBoard, ArgsTestUboot, Axvisor, TestCommand, build};
use crate::{
    context::{AxvisorCliArgs, SnapshotPersistence},
    test::{board as board_test, qemu as test_qemu, suite as test_suite},
};

const AXVISOR_TEST_SUITE_OS: &str = "axvisor";
const AXVISOR_NORMAL_GROUP: &str = "normal";
pub(crate) const AXVISOR_QEMU_TESTS_MOVED_MESSAGE: &str = "Axvisor QEMU loader tests moved to \
                                                           `cargo axloader test qemu`; use `cargo \
                                                           axloader test qemu --arch <arch>`";

pub(super) async fn test(axvisor: &mut Axvisor, args: ArgsTest) -> anyhow::Result<()> {
    match args.command {
        TestCommand::Qemu(_) => bail!(AXVISOR_QEMU_TESTS_MOVED_MESSAGE),
        TestCommand::Uboot(args) => axvisor.test_uboot(args).await,
        TestCommand::Board(args) => axvisor.test_board(args).await,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoardTestGroup {
    pub(crate) name: String,
    pub(crate) board_name: String,
    pub(crate) build_config: PathBuf,
    pub(crate) board_test_config_path: PathBuf,
}

impl board_test::BoardTestGroupInfo for BoardTestGroup {
    fn name(&self) -> &str {
        &self.name
    }

    fn board_name(&self) -> &str {
        &self.board_name
    }
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    group: &str,
    selected_case: Option<&str>,
    board: Option<&str>,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let test_suite_dir = test_suite_dir(workspace_root, group)?;
    let groups = collect_board_test_groups(workspace_root, &test_suite_dir)?;
    board_test::filter_board_test_groups(groups, selected_case, board, "axvisor", || {
        format!(
            "no Axvisor board test groups found under {}",
            test_suite_dir.display()
        )
    })
}

fn collect_board_test_groups(
    workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<BoardTestGroup>> {
    let mut groups = Vec::new();
    for info in board_test::discover_board_case_build_infos(test_suite_dir, "Axvisor")? {
        ensure_board_run_config(&info.board_test_config_path)?;
        let build_config = resolve_workspace_path(workspace_root, info.build_config_path);
        ensure_file_exists(&build_config, "Axvisor board build group config")?;
        groups.push(BoardTestGroup {
            name: info.name,
            board_name: info.board_name,
            build_config,
            board_test_config_path: info.board_test_config_path,
        });
    }

    Ok(groups)
}

fn discover_uboot_test_group(
    workspace_root: &Path,
    board: &str,
    guest: &str,
) -> anyhow::Result<BoardTestGroup> {
    let board_name = format!("{board}-{guest}");
    let mut groups = discover_board_test_groups(
        workspace_root,
        AXVISOR_NORMAL_GROUP,
        None,
        Some(&board_name),
    )?;

    if groups.len() == 1 {
        return Ok(groups.remove(0));
    }

    let labels = groups
        .iter()
        .map(|group| format!("{}/{}", group.name, group.board_name))
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "ambiguous axvisor uboot test target board=`{board}` guest=`{guest}`. Matching cases are: \
         {labels}"
    )
}

fn merge_board_test_uboot_config(
    base: Option<UbootConfig>,
    board_test: ostool::board::config::BoardRunConfig,
) -> UbootConfig {
    let mut uboot = base.unwrap_or_default();
    let test_uboot = UbootConfig::from_board_run_config(&board_test);
    if test_uboot.dtb_file.is_some() {
        uboot.dtb_file = test_uboot.dtb_file;
    }
    if test_uboot.kernel_load_addr.is_some() {
        uboot.kernel_load_addr = test_uboot.kernel_load_addr;
    }
    if test_uboot.fit_load_addr.is_some() {
        uboot.fit_load_addr = test_uboot.fit_load_addr;
    }
    if test_uboot.bootm_addr.is_some() {
        uboot.bootm_addr = test_uboot.bootm_addr;
    }
    uboot.success_regex = test_uboot.success_regex;
    uboot.fail_regex = test_uboot.fail_regex;
    uboot.uboot_cmd = test_uboot.uboot_cmd;
    uboot.shell_prefix = test_uboot.shell_prefix;
    uboot.shell_init_cmd = test_uboot.shell_init_cmd;
    if test_uboot.timeout.is_some() {
        uboot.timeout = test_uboot.timeout;
    }
    uboot
}

fn ensure_board_run_config(path: &Path) -> anyhow::Result<()> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str::<ostool::board::config::BoardRunConfig>(&content)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(())
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
    test_suite::require_group_dir(workspace_root, AXVISOR_TEST_SUITE_OS, "Axvisor", group)
}

fn test_suite_root(workspace_root: &Path) -> PathBuf {
    test_suite::suite_root(workspace_root, AXVISOR_TEST_SUITE_OS)
}

fn discover_test_group_names(workspace_root: &Path) -> anyhow::Result<Vec<String>> {
    test_suite::discover_group_names(workspace_root, AXVISOR_TEST_SUITE_OS)
}

impl Axvisor {
    pub(super) async fn test_uboot(&mut self, args: ArgsTestUboot) -> anyhow::Result<()> {
        let group = discover_uboot_test_group(self.app.workspace_root(), &args.board, &args.guest)?;
        let explicit_uboot_config = args.uboot_config.clone();
        let uboot_config_summary = explicit_uboot_config
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "using test-suit board config only".to_string());
        let board_test_config = group.board_test_config_path.clone();
        let board_test_config_summary = board_test_config.display().to_string();

        if let Some(path) = explicit_uboot_config.as_ref()
            && !path.exists()
        {
            bail!(
                "missing explicit U-Boot config `{}` for axvisor board tests",
                path.display()
            );
        }

        println!(
            "running axvisor uboot test for board: {} guest: {} case: {}",
            args.board, args.guest, group.name
        );

        let request = self.prepare_request(
            axvisor_board_test_build_args(&group),
            None,
            explicit_uboot_config.clone(),
            SnapshotPersistence::Discard,
        )?;

        let cargo = build::load_cargo_config(&request)?;
        let base_uboot = match request.uboot_config.as_deref() {
            Some(_) => self.load_uboot_config(&request, &cargo).await?,
            None => Some(self.app.ensure_uboot_config_for_cargo(&cargo).await?),
        };
        let board_config = self
            .load_board_config(&cargo, Some(board_test_config.as_path()))
            .await?;
        let uboot = Some(merge_board_test_uboot_config(base_uboot, board_config));
        self.app
            .uboot(cargo, request.build_info_path, uboot)
            .await
            .with_context(|| {
                format!(
                    "axvisor uboot test failed for board `{}` guest `{}` case `{}` \
                     (build_config={}, board_test_config={}, uboot_config={})",
                    args.board,
                    args.guest,
                    group.name,
                    group.build_config.display(),
                    board_test_config_summary,
                    uboot_config_summary
                )
            })
    }

    pub(super) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        if args.list && args.test_group.is_none() {
            let groups = discover_test_group_names(self.app.workspace_root())?
                .into_iter()
                .filter_map(|group| {
                    match discover_board_test_groups(
                        self.app.workspace_root(),
                        &group,
                        args.test_case.as_deref(),
                        args.board.as_deref(),
                    ) {
                        Ok(groups) if groups.is_empty() => None,
                        Ok(groups) => Some(Ok((group, board_test::labeled_board_cases(groups)))),
                        Err(err) => {
                            let message = err.to_string();
                            if message.starts_with("no Axvisor ") {
                                None
                            } else {
                                Some(Err(err))
                            }
                        }
                    }
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            if groups.is_empty() {
                bail!(
                    "no Axvisor board test groups found under {}",
                    test_suite_root(self.app.workspace_root()).display()
                );
            }
            println!(
                "{}",
                test_qemu::render_labeled_case_forest("axvisor", groups)
            );
            return Ok(());
        }

        let test_group = args.test_group.as_deref().unwrap_or(AXVISOR_NORMAL_GROUP);
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            test_group,
            args.test_case.as_deref(),
            args.board.as_deref(),
        )?;
        if args.list {
            let case_names = board_test::labeled_board_cases(groups);
            println!(
                "{}",
                test_qemu::render_labeled_case_forest("axvisor", [(test_group, case_names)])
            );
            return Ok(());
        }

        let mut run_state = board_test::BoardTestRunState::new("axvisor", groups.len());
        for (index, group) in groups.into_iter().enumerate() {
            let group_label = run_state.start_group(index, &group);
            let board_test_config = group.board_test_config_path.clone();
            let board_test_config_summary = board_test_config.display().to_string();
            if !board_test_config.exists() {
                run_state.fail_group(
                    group_label,
                    anyhow::anyhow!("missing board test config `{board_test_config_summary}`"),
                );
                continue;
            }

            let result = async {
                let request = self.prepare_request(
                    axvisor_board_test_build_args(&group),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                let cargo = build::load_cargo_config(&request)?;
                let board_config = self
                    .load_board_config(&cargo, Some(board_test_config.as_path()))
                    .await?;
                self.app
                    .board(
                        cargo,
                        request.build_info_path,
                        board_config,
                        RunBoardOptions {
                            board_type: args.board_type.clone(),
                            server: args.server.clone(),
                            port: args.port,
                        },
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "axvisor board test failed for group `{}` (build_config={}, \
                             board_test_config={})",
                            group_label,
                            group.build_config.display(),
                            board_test_config_summary
                        )
                    })
            }
            .await;

            match result {
                Ok(()) => run_state.pass_group(&group_label),
                Err(err) => run_state.fail_group(group_label, err),
            }
        }
        run_state.finish()
    }
}

fn axvisor_board_test_build_args(group: &BoardTestGroup) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(group.build_config.clone()),
        arch: None,
        target: None,
        plat_dyn: None,
        smp: None,
        debug: false,
        vmconfigs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[derive(serde::Deserialize)]
    struct TestBuildConfigVmConfigs {
        #[serde(default)]
        vm_configs: Vec<PathBuf>,
    }

    fn write_qemu_config(root: &Path, case: &str, arch: &str, body: &str) -> PathBuf {
        write_qemu_config_in_group(root, "normal", "default", case, arch, body)
    }

    fn write_qemu_config_in_group(
        root: &Path,
        group: &str,
        build_group: &str,
        case: &str,
        arch: &str,
        body: &str,
    ) -> PathBuf {
        let dir = root
            .join("test-suit/axvisor")
            .join(group)
            .join(build_group)
            .join(case);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("qemu-{arch}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    fn write_qemu_build_config(
        root: &Path,
        group: &str,
        build_group: &str,
        target: &str,
    ) -> PathBuf {
        let dir = root.join("test-suit/axvisor").join(group).join(build_group);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("build-{target}.toml"));
        fs::write(
            &path,
            format!("target = \"{target}\"\nfeatures = []\nlog = \"Info\"\nvm_configs = []\n"),
        )
        .unwrap();
        path
    }

    fn write_board_build_config(root: &Path, build_group: &str) -> PathBuf {
        write_qemu_build_config(
            root,
            "normal",
            build_group,
            "aarch64-unknown-none-softfloat",
        )
    }

    fn write_board_config(root: &Path, case: &str, name: &str, body: &str) -> PathBuf {
        write_board_config_in_group(root, "normal", "default", case, name, body)
    }

    fn write_board_config_in_group(
        root: &Path,
        group: &str,
        build_group: &str,
        case: &str,
        name: &str,
        body: &str,
    ) -> PathBuf {
        let dir = root
            .join("test-suit/axvisor")
            .join(group)
            .join(build_group)
            .join(case);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("board-{name}.toml"));
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn checked_in_test_build_vmconfigs_exist() {
        let workspace_root = std::env::current_dir().unwrap();
        let axvisor_suite = workspace_root.join("test-suit/axvisor");
        if !axvisor_suite.is_dir() {
            return;
        }

        let mut stack = vec![axvisor_suite];
        let mut checked = 0;
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }

                let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                if !file_name.starts_with("build-")
                    || path.extension().and_then(|ext| ext.to_str()) != Some("toml")
                {
                    continue;
                }

                let content = fs::read_to_string(&path).unwrap();
                let config: TestBuildConfigVmConfigs = toml::from_str(&content).unwrap();
                for vm_config in config.vm_configs {
                    if vm_config.starts_with("os/axvisor/tmp/vmconfigs") {
                        continue;
                    }
                    checked += 1;
                    let vm_config_path = if vm_config.is_absolute() {
                        vm_config
                    } else {
                        workspace_root.join(vm_config)
                    };
                    assert!(
                        vm_config_path.is_file(),
                        "{} references missing vm_config {}",
                        path.display(),
                        vm_config_path.display()
                    );
                }
            }
        }

        assert!(checked > 0);
    }

    #[test]
    fn axvisor_qemu_test_entry_reports_axloader_migration() {
        assert!(AXVISOR_QEMU_TESTS_MOVED_MESSAGE.contains("cargo axloader test qemu"));
        assert!(AXVISOR_QEMU_TESTS_MOVED_MESSAGE.contains("--arch <arch>"));
    }

    #[test]
    fn returns_all_board_test_groups_when_no_filter_is_given() {
        let root = tempdir().unwrap();
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "board_type = \"OrangePi-5-Plus\"\n",
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
    fn discovers_board_case_when_case_dir_contains_build_config() {
        let root = tempdir().unwrap();
        let case_dir = root.path().join("test-suit/axvisor/normal/smoke");
        fs::create_dir_all(&case_dir).unwrap();
        let build_config = case_dir.join("build-aarch64-unknown-none-softfloat.toml");
        fs::write(
            &build_config,
            "target = \"aarch64-unknown-none-softfloat\"\n",
        )
        .unwrap();
        let board_test_config = case_dir.join("board-phytiumpi-linux.toml");
        fs::write(&board_test_config, "board_type = \"PhytiumPi\"\n").unwrap();

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "phytiumpi-linux");
        assert_eq!(groups[0].build_config, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn board_case_uses_unique_nearest_build_config_without_target_assumption() {
        let root = tempdir().unwrap();
        let wrapper_dir = root.path().join("test-suit/axvisor/normal/board-custom");
        let case_dir = wrapper_dir.join("smoke");
        fs::create_dir_all(&case_dir).unwrap();
        let build_config = wrapper_dir.join("build-riscv64gc-unknown-none-elf.toml");
        fs::write(&build_config, "target = \"riscv64gc-unknown-none-elf\"\n").unwrap();
        let board_test_config = case_dir.join("board-custom.toml");
        fs::write(&board_test_config, "board_type = \"Custom\"\n").unwrap();

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "custom");
        assert_eq!(groups[0].build_config, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_group_by_case() {
        let root = tempdir().unwrap();
        let build_config = write_board_build_config(root.path(), "default");
        let board_test_config = write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );

        let groups =
            discover_board_test_groups(root.path(), "normal", Some("smoke"), None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "phytiumpi-linux");
        assert_eq!(groups[0].build_config, build_config);
        assert_eq!(groups[0].board_test_config_path, board_test_config);
    }

    #[test]
    fn filters_board_test_groups_by_board() {
        let root = tempdir().unwrap();
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "syscall",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
        );
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "board_type = \"OrangePi-5-Plus\"\n",
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
    fn discovers_uboot_test_group_from_board_cases() {
        let root = tempdir().unwrap();
        let build_config = write_board_build_config(root.path(), "board-rdk-s100");
        let board_test_config = write_board_config_in_group(
            root.path(),
            "normal",
            "board-rdk-s100",
            "smoke",
            "rdk-s100-linux",
            "board_type = \"RDK-S100\"\nuboot_cmd = [\"run ab_select_cmd\", \"run \
             avb_boot\"]\nsuccess_regex = [\"ubuntu login:\"]\nfail_regex = [\"(?i)panic\"]\n",
        );

        let group = discover_uboot_test_group(root.path(), "rdk-s100", "linux").unwrap();

        assert_eq!(group.name, "smoke");
        assert_eq!(group.board_name, "rdk-s100-linux");
        assert_eq!(group.build_config, build_config);
        assert_eq!(group.board_test_config_path, board_test_config);
    }

    #[test]
    fn uboot_test_config_uses_board_case_matchers_and_keeps_base_local_config() {
        let base = UbootConfig {
            dtb_file: Some("${env:BOARD_DTB}".to_string()),
            success_regex: vec!["old-ok".to_string()],
            fail_regex: vec!["old-fail".to_string()],
            uboot_cmd: Some(vec!["old-boot".to_string()]),
            shell_prefix: Some("old-login:".to_string()),
            timeout: Some(300),
            local: ostool::run::uboot::LocalUbootConfig {
                serial: Some("/dev/ttyUSB1".to_string()),
                baud_rate: Some("1500000".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let board_test = ostool::board::config::BoardRunConfig {
            board_type: "RDK-S100".to_string(),
            success_regex: vec!["ubuntu login:".to_string()],
            fail_regex: vec!["(?i)panic".to_string()],
            uboot_cmd: Some(vec![
                "run ab_select_cmd".to_string(),
                "run avb_boot".to_string(),
            ]),
            kernel_load_addr: Some("0x200000".to_string()),
            fit_load_addr: Some("0x2000000".to_string()),
            bootm_addr: Some("0x2000000".to_string()),
            shell_prefix: Some("ubuntu login:".to_string()),
            ..Default::default()
        };

        let merged = merge_board_test_uboot_config(Some(base), board_test);

        assert_eq!(merged.success_regex, vec!["ubuntu login:"]);
        assert_eq!(merged.fail_regex, vec!["(?i)panic"]);
        assert_eq!(
            merged.uboot_cmd,
            Some(vec![
                "run ab_select_cmd".to_string(),
                "run avb_boot".to_string()
            ])
        );
        assert_eq!(merged.shell_prefix.as_deref(), Some("ubuntu login:"));
        assert_eq!(merged.dtb_file.as_deref(), Some("${env:BOARD_DTB}"));
        assert_eq!(merged.kernel_load_addr.as_deref(), Some("0x200000"));
        assert_eq!(merged.fit_load_addr.as_deref(), Some("0x2000000"));
        assert_eq!(merged.bootm_addr.as_deref(), Some("0x2000000"));
        assert_eq!(merged.timeout, Some(300));
        assert_eq!(merged.local.serial.as_deref(), Some("/dev/ttyUSB1"));
        assert_eq!(merged.local.baud_rate.as_deref(), Some("1500000"));
    }

    #[test]
    fn x86_linux_direct_boot_configs_keep_timer_calibration_bypass() {
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for path in [
            "os/axvisor/configs/vms/qemu/x86_64/linux-vmx-smp1.toml",
            "os/axvisor/configs/vms/qemu/x86_64/linux-svm-smp1.toml",
        ] {
            let content = fs::read_to_string(workspace_root.join(path)).unwrap();
            assert!(
                content.contains("no_timer_check"),
                "{path} should keep no_timer_check to avoid x86 Linux guest timer calibration \
                 stalls"
            );
        }
    }

    #[test]
    fn ignores_qemu_only_build_groups_when_discovering_board_tests() {
        let root = tempdir().unwrap();
        write_qemu_build_config(
            root.path(),
            "normal",
            "qemu",
            "aarch64-unknown-none-softfloat",
        );
        write_qemu_build_config(root.path(), "normal", "qemu", "x86_64-unknown-none");
        write_qemu_config(
            root.path(),
            "smoke",
            "aarch64",
            "shell_prefix = \"~ #\"\nshell_init_cmd = \"pwd\"\nsuccess_regex = []\nfail_regex = \
             []\n",
        );

        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "orangepi-5-plus-linux",
            "board_type = \"OrangePi-5-Plus\"\n",
        );

        let groups = discover_board_test_groups(root.path(), "normal", None, None).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "smoke");
        assert_eq!(groups[0].board_name, "orangepi-5-plus-linux");
    }

    #[test]
    fn rejects_unknown_board_test_board() {
        let root = tempdir().unwrap();
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
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
        write_board_build_config(root.path(), "default");
        write_board_config(
            root.path(),
            "smoke",
            "phytiumpi-linux",
            "board_type = \"PhytiumPi\"\n",
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
    fn rejects_empty_board_test_group() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("test-suit/axvisor/empty")).unwrap();

        let err = discover_board_test_groups(root.path(), "empty", None, None).unwrap_err();

        assert!(
            err.to_string()
                .contains("no Axvisor board test groups found under")
        );
    }

    #[test]
    fn board_case_config_is_also_valid_board_run_config() {
        let config: ostool::board::config::BoardRunConfig = toml::from_str(
            "board_type = \"PhytiumPi\"\nshell_prefix = \"login:\"\nshell_init_cmd = \
             \"root\"\nsuccess_regex = [\"(?m)^root@.*#\\\\s*$\"]\n",
        )
        .unwrap();

        assert_eq!(config.board_type, "PhytiumPi");
        assert_eq!(config.shell_prefix.as_deref(), Some("login:"));
    }
}
