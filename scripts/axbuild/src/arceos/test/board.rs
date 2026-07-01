use std::path::Path;

use anyhow::Context;
use ostool::board::RunBoardOptions;

use super::{ARCEOS_TEST_SUITE_OS, ArgsTestBoard, types::ArceosBoardTestGroup};
use crate::{
    arceos::ArceOS,
    context::{BuildCliArgs, SnapshotPersistence, arch_for_target_checked},
    test::{board as board_test, qemu as qemu_test, suite as test_suite},
};

pub(crate) fn collect_board_test_groups(
    _workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<ArceosBoardTestGroup>> {
    let mut groups = Vec::new();
    for info in board_test::discover_board_case_build_infos(test_suite_dir, "ArceOS")? {
        let build_file = crate::arceos::board::load_build_file(&info.build_config_path)
            .with_context(|| {
                format!(
                    "failed to load ArceOS board build config `{}`",
                    info.build_config_path.display()
                )
            })?;
        let package = build_file.package.with_context(|| {
            format!(
                "ArceOS board build config `{}` must set `package`",
                info.build_config_path.display()
            )
        })?;
        let target = build_file.target.with_context(|| {
            format!(
                "ArceOS board build config `{}` must set `target`",
                info.build_config_path.display()
            )
        })?;
        let arch = arch_for_target_checked(&target)?.to_string();
        groups.push(ArceosBoardTestGroup {
            name: info.name,
            board_name: info.board_name,
            package,
            arch,
            target,
            build_config_path: info.build_config_path,
            board_test_config_path: info.board_test_config_path,
        });
    }

    Ok(groups)
}

pub(crate) fn discover_board_test_groups(
    workspace_root: &Path,
    selected_case: Option<&str>,
    selected_board: Option<&str>,
) -> anyhow::Result<Vec<ArceosBoardTestGroup>> {
    let suite_root = test_suite::suite_root(workspace_root, ARCEOS_TEST_SUITE_OS);
    let mut groups = Vec::new();
    for group in test_suite::discover_group_names(workspace_root, ARCEOS_TEST_SUITE_OS)? {
        let group_dir = test_suite::group_dir(workspace_root, ARCEOS_TEST_SUITE_OS, &group);
        groups.extend(collect_board_test_groups(workspace_root, &group_dir)?);
    }

    board_test::filter_board_test_groups(groups, selected_case, selected_board, "ArceOS", || {
        format!(
            "no ArceOS board test groups found under {}",
            suite_root.display()
        )
    })
}

impl ArceOS {
    pub(super) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            args.test_case.as_deref(),
            args.board.as_deref(),
        )?;
        if args.list {
            let case_names = board_test::labeled_board_cases(groups);
            println!(
                "{}",
                qemu_test::render_labeled_case_forest("arceos", [("board", case_names)])
            );
            return Ok(());
        }

        let mut run_state = board_test::BoardTestRunState::new("arceos", groups.len());
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
                    test_board_build_args(&group),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                self.run_board_request(
                    request,
                    Some(board_test_config.clone()),
                    RunBoardOptions {
                        board_type: args.board_type.clone(),
                        server: args.server.clone(),
                        port: args.port,
                    },
                )
                .await
                .with_context(|| {
                    format!(
                        "arceos board test failed for group `{}` (build_config={}, \
                         board_test_config={})",
                        group_label,
                        group.build_config_path.display(),
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

fn test_board_build_args(group: &ArceosBoardTestGroup) -> BuildCliArgs {
    BuildCliArgs {
        config: Some(group.build_config_path.clone()),
        package: Some(group.package.clone()),
        arch: None,
        target: Some(group.target.clone()),
        plat_dyn: Some(true),
        smp: None,
        debug: false,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use tempfile::tempdir;

    use super::*;

    fn write_board_group(root: &Path) {
        let group = root.join("test-suit/arceos/board-orangepi-5-plus");
        let boot = group.join("boot");
        fs::create_dir_all(&boot).unwrap();
        fs::write(
            group.join("build-aarch64-unknown-none-softfloat.toml"),
            r#"
package = "arceos-helloworld"
target = "aarch64-unknown-none-softfloat"
plat_dyn = true
features = []
log = "Info"
max_cpu_num = 1
"#,
        )
        .unwrap();
        fs::write(
            boot.join("board-orangepi-5-plus.toml"),
            r#"
board_type = "OrangePi-5-Plus"
success_regex = ["Hello, world!"]
fail_regex = ["(?i)panic"]
"#,
        )
        .unwrap();
    }

    #[test]
    fn collect_board_test_groups_reads_package_and_target_from_build_config() {
        let root = tempdir().unwrap();
        write_board_group(root.path());
        let group_dir = root.path().join("test-suit/arceos/board-orangepi-5-plus");

        let groups = collect_board_test_groups(root.path(), &group_dir).unwrap();

        assert_eq!(groups.len(), 1);
        let group = &groups[0];
        assert_eq!(group.name, "boot");
        assert_eq!(group.board_name, "orangepi-5-plus");
        assert_eq!(group.package, "arceos-helloworld");
        assert_eq!(group.arch, "aarch64");
        assert_eq!(group.target, "aarch64-unknown-none-softfloat");
        assert_eq!(
            group.build_config_path,
            group_dir.join("build-aarch64-unknown-none-softfloat.toml")
        );
        assert_eq!(
            group.board_test_config_path,
            group_dir.join("boot/board-orangepi-5-plus.toml")
        );
    }

    #[test]
    fn discover_board_test_groups_filters_by_case_and_board() {
        let root = tempdir().unwrap();
        write_board_group(root.path());

        let groups =
            discover_board_test_groups(root.path(), Some("boot"), Some("orangepi-5-plus")).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "boot");
        assert_eq!(groups[0].board_name, "orangepi-5-plus");
    }
}
