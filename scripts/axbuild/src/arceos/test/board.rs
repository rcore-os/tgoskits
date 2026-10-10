use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use ostool::{board::RunBoardOptions, build::config::Cargo};

use super::{
    ARCEOS_AXTEST_GROUP, ARCEOS_TEST_SUITE_OS, ArgsTestBoard, types::ArceosBoardTestGroup,
};
use crate::{
    arceos::{ArceOS, build},
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
    selected_cases: &[String],
    selected_board: Option<&str>,
) -> anyhow::Result<Vec<ArceosBoardTestGroup>> {
    let suite_root = test_suite::suite_root(workspace_root, ARCEOS_TEST_SUITE_OS);
    let mut groups = Vec::new();
    for group in test_suite::discover_group_names(workspace_root, ARCEOS_TEST_SUITE_OS)? {
        if group == ARCEOS_AXTEST_GROUP {
            continue;
        }
        let group_dir = test_suite::group_dir(workspace_root, ARCEOS_TEST_SUITE_OS, &group);
        groups.extend(collect_board_test_groups(workspace_root, &group_dir)?);
    }

    board_test::filter_board_test_groups_by_names(
        groups,
        selected_cases,
        selected_board,
        "ArceOS",
        || {
            format!(
                "no ArceOS board test groups found under {}",
                suite_root.display()
            )
        },
    )
}

struct PreparedBoardBuild {
    cargo: Cargo,
    request: crate::context::ResolvedBuildRequest,
    elf_path: PathBuf,
}

struct PreparedBoardBuildGroup {
    cargo: Option<Cargo>,
    mode: Option<build::ArceosBuildMode>,
    result: Result<PreparedBoardBuild, String>,
}

impl ArceOS {
    pub(super) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            &args.test_case,
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

        let mut build_configs = Vec::<(PathBuf, Vec<usize>)>::new();
        for (position, (_, group)) in runnable.iter().enumerate() {
            let build_config = group.build_config_path.canonicalize().with_context(|| {
                format!(
                    "failed to resolve ArceOS board build config `{}`",
                    group.build_config_path.display()
                )
            })?;
            if let Some((_, positions)) = build_configs
                .iter_mut()
                .find(|(config, _)| *config == build_config)
            {
                positions.push(position);
            } else {
                build_configs.push((build_config, vec![position]));
            }
        }

        // Board test TOMLs only select runtime checks. Preserve each unique
        // kernel ELF before the next Cargo build reuses the common artifact.
        let artifact_parent = self.app.target_dir().join("axbuild");
        fs::create_dir_all(&artifact_parent).with_context(|| {
            format!(
                "failed to create ArceOS board artifact parent {}",
                artifact_parent.display()
            )
        })?;
        let artifact_directory = tempfile::Builder::new()
            .prefix("arceos-board-artifacts-")
            .tempdir_in(&artifact_parent)
            .context("failed to create temporary ArceOS board artifact directory")?;
        let mut prepared_builds = Vec::<PreparedBoardBuildGroup>::new();
        let mut config_to_build = HashMap::<PathBuf, usize>::new();
        for (build_config, positions) in build_configs {
            let group = &runnable[positions[0]].1;
            let prepared = async {
                let request = self.prepare_request(
                    test_board_build_args(group),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
                Self::validate_board_request(&request)?;
                self.app.set_debug_mode(request.debug)?;
                let mode = build::load_arceos_build_mode(&request.build_info_path)?;
                let cargo = match &mode {
                    build::ArceosBuildMode::Rust => {
                        build::load_cargo_config(&request, self.app.workspace_context())?
                    }
                    build::ArceosBuildMode::AppC { .. } => {
                        build::load_c_app_cargo_config(&request, self.app.workspace_context())?
                    }
                };
                Ok::<_, anyhow::Error>((request, mode, cargo))
            }
            .await;
            let (request, mode, cargo) = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    let index = prepared_builds.len();
                    prepared_builds.push(PreparedBoardBuildGroup {
                        cargo: None,
                        mode: None,
                        result: Err(format!("{error:#}")),
                    });
                    config_to_build.insert(build_config, index);
                    continue;
                }
            };
            if let Some(index) = prepared_builds.iter().position(|existing| {
                existing.cargo.as_ref() == Some(&cargo)
                    && existing.mode.as_ref() == Some(&mode)
                    && existing.result.is_ok()
            }) {
                config_to_build.insert(build_config, index);
                continue;
            }
            let result = async {
                let elf_path = match &mode {
                    build::ArceosBuildMode::Rust => {
                        let output = self
                            .app
                            .build(cargo.clone(), request.build_info_path.clone())
                            .await?;
                        output.elf_path().to_path_buf()
                    }
                    build::ArceosBuildMode::AppC { app_dir, app_name } => {
                        let output =
                            self.build_c_app_request(&request, app_dir.clone(), app_name.clone())?;
                        output.elf_path
                    }
                };
                let elf_path = qemu_test::preserve_build_artifact(
                    &elf_path,
                    artifact_directory.path(),
                    prepared_builds.len(),
                )?;
                Ok::<_, anyhow::Error>(PreparedBoardBuild {
                    cargo: cargo.clone(),
                    request,
                    elf_path,
                })
            }
            .await;
            let index = prepared_builds.len();
            prepared_builds.push(PreparedBoardBuildGroup {
                cargo: Some(cargo),
                mode: Some(mode),
                result: result.map_err(|error| format!("{error:#}")),
            });
            config_to_build.insert(build_config, index);
        }

        for (group_label, group) in runnable {
            let board_test_config = group.board_test_config_path.clone();
            let board_test_config_summary = board_test_config.display().to_string();
            let build_config = match group.build_config_path.canonicalize() {
                Ok(path) => path,
                Err(error) => {
                    run_state.fail_group(
                        group_label,
                        anyhow::anyhow!(
                            "failed to resolve ArceOS board build config `{}`: {error}",
                            group.build_config_path.display()
                        ),
                    );
                    continue;
                }
            };
            let Some(build_index) = config_to_build.get(&build_config) else {
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
                let board_config = self
                    .load_board_config(&prepared.cargo, Some(board_test_config.as_path()))
                    .await?;
                let mut request = prepared.request.clone();
                request.build_info_path = group.build_config_path.clone();
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
            discover_board_test_groups(root.path(), &["boot".to_string()], Some("orangepi-5-plus"))
                .unwrap();

        assert_eq!(groups[0].name, "boot");
        assert_eq!(groups[0].board_name, "orangepi-5-plus");
    }
}
