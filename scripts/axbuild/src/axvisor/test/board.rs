use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::PathBuf,
};

use anyhow::Context;
use ostool::{board::RunBoardOptions, build::config::Cargo, run::uboot::UbootConfig};

use super::{
    AXVISOR_NORMAL_GROUP, BoardTestGroup, discover_board_test_groups,
    discovery::{discover_test_group_names, discover_uboot_test_group, suite_roots_label},
};
use crate::{
    axvisor::{ArgsTestBoard, ArgsTestUboot, Axvisor, build},
    context::{AxvisorCliArgs, ResolvedAxvisorRequest, SnapshotPersistence},
    test::{board as board_test, qemu as test_qemu},
};

struct PreparedBoardBuild {
    cargo: Cargo,
    request: ResolvedAxvisorRequest,
    elf_path: PathBuf,
}

struct PreparedBoardBuildGroup {
    cargo: Cargo,
    result: Result<PreparedBoardBuild, String>,
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
            anyhow::bail!(
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
        let mut request = Self::board_test_request(request);

        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        let base_uboot = match request.uboot_config.as_deref() {
            Some(_) => self.load_uboot_config(&request, &cargo).await?,
            None => Some(self.app.ensure_uboot_config_for_cargo(&cargo).await?),
        };
        let mut board_config = self
            .load_board_config(&cargo, Some(board_test_config.as_path()))
            .await?;
        self.prepare_guest_payload(&mut request, &mut board_config.boot, true)
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
                        &args.test_case,
                        &args.board,
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
                anyhow::bail!(
                    "no Axvisor board test groups found under {}",
                    suite_roots_label(self.app.workspace_root())
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
            &args.test_case,
            &args.board,
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
        let mut runnable = Vec::new();
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

            runnable.push((group_label, group));
        }

        let mut build_groups = BTreeMap::<PathBuf, Vec<usize>>::new();
        let mut resolved_build_configs = HashMap::<usize, PathBuf>::new();
        let mut build_config_errors = HashMap::<usize, String>::new();
        for (position, (_, group)) in runnable.iter().enumerate() {
            match group.build_config.canonicalize() {
                Ok(build_config) => {
                    resolved_build_configs.insert(position, build_config.clone());
                    build_groups.entry(build_config).or_default().push(position);
                }
                Err(error) => {
                    build_config_errors.insert(
                        position,
                        format!(
                            "failed to resolve Axvisor board build config `{}`: {error}",
                            group.build_config.display()
                        ),
                    );
                }
            }
        }

        // Board test TOMLs only select runtime payloads and checks. Build each
        // unique host kernel once and preserve its ELF before the next build.
        let artifact_parent = self.app.target_dir().join("axbuild");
        fs::create_dir_all(&artifact_parent).with_context(|| {
            format!(
                "failed to create Axvisor board artifact parent {}",
                artifact_parent.display()
            )
        })?;
        let artifact_directory = tempfile::Builder::new()
            .prefix("axvisor-board-artifacts-")
            .tempdir_in(&artifact_parent)
            .context("failed to create temporary Axvisor board artifact directory")?;
        let mut prepared_builds = Vec::<PreparedBoardBuildGroup>::new();
        let mut config_to_build = HashMap::<PathBuf, usize>::new();
        for (build_config, positions) in build_groups {
            let group = &runnable[positions[0]].1;
            let prepared = async {
                let request = self.prepare_request(
                    axvisor_board_test_build_args(group),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                let request = Self::board_test_request(request);
                self.app.set_debug_mode(request.debug)?;
                let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
                Ok::<_, anyhow::Error>((request, cargo))
            }
            .await;
            let (request, cargo) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    let index = prepared_builds.len();
                    prepared_builds.push(PreparedBoardBuildGroup {
                        cargo: Default::default(),
                        result: Err(format!("{error:#}")),
                    });
                    config_to_build.insert(build_config, index);
                    continue;
                }
            };
            if let Some(index) = prepared_builds
                .iter()
                .position(|existing| existing.result.is_ok() && existing.cargo == cargo)
            {
                config_to_build.insert(build_config, index);
                continue;
            }

            let result_cargo = cargo.clone();
            let result = async {
                let output = self
                    .app
                    .build(cargo.clone(), request.build_info_path.clone())
                    .await?;
                let elf_path = test_qemu::preserve_build_artifact(
                    output.elf_path(),
                    artifact_directory.path(),
                    prepared_builds.len(),
                )?;
                Ok::<_, anyhow::Error>(PreparedBoardBuild {
                    cargo: result_cargo,
                    request,
                    elf_path,
                })
            }
            .await;
            let index = prepared_builds.len();
            prepared_builds.push(PreparedBoardBuildGroup {
                cargo: cargo.clone(),
                result: result.map_err(|error| format!("{error:#}")),
            });
            config_to_build.insert(build_config, index);
        }

        for (position, (group_label, group)) in runnable.into_iter().enumerate() {
            let board_test_config = group.board_test_config_path.clone();
            let board_test_config_summary = board_test_config.display().to_string();
            if let Some(error) = build_config_errors.get(&position) {
                run_state.fail_group(group_label, anyhow::anyhow!("{error}"));
                continue;
            }
            let Some(build_config) = resolved_build_configs.get(&position) else {
                run_state.fail_group(
                    group_label,
                    anyhow::anyhow!(
                        "missing resolved build config for `{}`",
                        group.build_config.display()
                    ),
                );
                continue;
            };
            let Some(build_index) = config_to_build.get(build_config) else {
                run_state.fail_group(
                    group_label,
                    anyhow::anyhow!("missing prepared build for `{}`", build_config.display()),
                );
                continue;
            };
            let prepared = match &prepared_builds[*build_index].result {
                Ok(prepared) => prepared,
                Err(error) => {
                    run_state.fail_group(
                        group_label,
                        anyhow::anyhow!("shared build failed for case: {error}"),
                    );
                    continue;
                }
            };

            let result = async {
                super::guest_build::prepare(&mut self.app, &board_test_config).await?;
                let mut request = prepared.request.clone();
                // Cargo identity is shared, but each board case keeps its own
                // build metadata and guest resource selection.
                request.build_info_path = group.build_config.clone();
                let mut board_config = self
                    .load_board_config(&prepared.cargo, Some(board_test_config.as_path()))
                    .await?;
                self.prepare_guest_payload(&mut request, &mut board_config.boot, true)
                    .await?;
                self.app
                    .board_prepared_elf(
                        prepared.elf_path.clone(),
                        prepared.cargo.to_bin,
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

    pub(super) fn board_test_request(
        mut request: ResolvedAxvisorRequest,
    ) -> ResolvedAxvisorRequest {
        request.smp = None;
        request
    }
}

fn merge_board_test_uboot_config(
    base: Option<UbootConfig>,
    board_test: ostool::board::config::BoardRunConfig,
) -> UbootConfig {
    let mut uboot = base.unwrap_or_default();
    let test_uboot = UbootConfig::from_board_run_config(&board_test);
    if test_uboot.boot.initramfs.is_some() {
        uboot.boot.initramfs = test_uboot.boot.initramfs;
    }
    if test_uboot.boot.cmdline.is_some() {
        uboot.boot.cmdline = test_uboot.boot.cmdline;
    }
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
    uboot.fail_regex = test_uboot.fail_regex;
    uboot.uboot_cmd = test_uboot.uboot_cmd;
    uboot.shell_check_steps = test_uboot.shell_check_steps;
    if test_uboot.timeout.is_some() {
        uboot.timeout = test_uboot.timeout;
    }
    uboot
}

fn axvisor_board_test_build_args(group: &BoardTestGroup) -> AxvisorCliArgs {
    AxvisorCliArgs {
        config: Some(group.build_config.clone()),
        arch: None,
        target: None,
        smp: None,
        debug: false,
        vmconfigs: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uboot_test_config_uses_board_case_matchers_and_keeps_base_local_config() {
        let base = UbootConfig {
            dtb_file: Some("${env:BOARD_DTB}".to_string()),
            fail_regex: vec!["old-fail".to_string()],
            uboot_cmd: Some(vec!["old-boot".to_string()]),
            shell_check_steps: vec![ostool::run::ShellCheckStep {
                shell_prefix: Some("old-login:".to_string()),
                shell_cmd: Some("old-command".to_string()),
                success_regex: Some(vec!["old-ok".to_string()]),
                ..Default::default()
            }],
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
            fail_regex: vec!["(?i)panic".to_string()],
            uboot_cmd: Some(vec![
                "run ab_select_cmd".to_string(),
                "run avb_boot".to_string(),
            ]),
            kernel_load_addr: Some("0x200000".to_string()),
            fit_load_addr: Some("0x2000000".to_string()),
            bootm_addr: Some("0x2000000".to_string()),
            shell_check_steps: vec![ostool::run::ShellCheckStep {
                shell_prefix: Some("ubuntu login:".to_string()),
                shell_cmd: Some("new-command".to_string()),
                success_regex: Some(vec!["ubuntu login:".to_string()]),
                ..Default::default()
            }],
            ..Default::default()
        };

        let expected_steps = board_test.shell_check_steps.clone();
        let merged = merge_board_test_uboot_config(Some(base), board_test);

        assert_eq!(
            merged.shell_check_steps[0].success_regex,
            Some(vec!["ubuntu login:".to_string()])
        );
        assert_eq!(merged.fail_regex, vec!["(?i)panic"]);
        assert_eq!(
            merged.uboot_cmd,
            Some(vec![
                "run ab_select_cmd".to_string(),
                "run avb_boot".to_string()
            ])
        );
        assert_eq!(merged.shell_check_steps, expected_steps);
        assert_eq!(merged.dtb_file.as_deref(), Some("${env:BOARD_DTB}"));
        assert_eq!(merged.kernel_load_addr.as_deref(), Some("0x200000"));
        assert_eq!(merged.fit_load_addr.as_deref(), Some("0x2000000"));
        assert_eq!(merged.bootm_addr.as_deref(), Some("0x2000000"));
        assert_eq!(merged.timeout, Some(300));
        assert_eq!(merged.local.serial.as_deref(), Some("/dev/ttyUSB1"));
        assert_eq!(merged.local.baud_rate.as_deref(), Some("1500000"));
    }
}
