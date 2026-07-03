use anyhow::Context;
use ostool::{board::RunBoardOptions, run::uboot::UbootConfig};

use super::{
    AXVISOR_NORMAL_GROUP, BoardTestGroup, discover_board_test_groups,
    discovery::{discover_test_group_names, discover_uboot_test_group, test_suite_root},
};
use crate::{
    axvisor::{ArgsTestBoard, ArgsTestUboot, Axvisor, build},
    context::{AxvisorCliArgs, SnapshotPersistence},
    test::{board as board_test, qemu as test_qemu},
};

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
                anyhow::bail!(
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
}
