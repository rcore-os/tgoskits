use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

use anyhow::Context;
use ostool::board::{BoardRunRequest, RunBoardOptions};

use super::{
    ArgsTestBoard, StarryBoardTestGroup,
    board_assets::{BoardSessionAssetRequest, prepare_board_session_assets},
    discover_board_test_groups,
};
use crate::{
    context::{
        RequestEnvironment, SnapshotPersistence, StarryCliArgs, arch_for_target_checked,
        board_run_request,
    },
    execution::{board as board_test, qemu as qemu_test},
    starry::{Starry, build},
};

struct PreparedBoardBuild {
    cargo: ostool::build::config::Cargo,
    elf_path: PathBuf,
}

struct PreparedBoardBuildGroup {
    cargo: ostool::build::config::Cargo,
    result: Result<PreparedBoardBuild, String>,
}

pub(crate) fn collect_board_test_groups(
    _workspace_root: &Path,
    test_suite_dir: &Path,
) -> anyhow::Result<Vec<StarryBoardTestGroup>> {
    let mut groups = Vec::new();
    for info in board_test::discover_board_case_build_infos(test_suite_dir, "Starry")? {
        let build_input = crate::build::load_config_input::<crate::starry::config::StarryConfig>(
            _workspace_root,
            Some(&info.build_config_path),
            None,
        )
        .with_context(|| {
            format!(
                "failed to load Starry board build config `{}`",
                info.build_config_path.display()
            )
        })?;
        let target = build_input
            .config
            .common
            .target
            .as_ref()
            .with_context(|| {
                format!(
                    "Starry board build config `{}` must set `target`",
                    info.build_config_path.display(),
                )
            })?
            .clone();
        let arch = arch_for_target_checked(&target)?.to_string();
        groups.push(StarryBoardTestGroup {
            shared_boot: board_test::board_test_shared_boot(
                info.board_test_config_path
                    .parent()
                    .context("board config has no parent")?,
                &info.board_name,
            )?,
            name: super::layout::board_case_id(&info.name),
            board_name: info.board_name,
            arch,
            target,
            build_config_path: info.build_config_path,
            build_input,
            board_test_config_path: info.board_test_config_path,
            required_env: info.required_env,
        });
    }

    Ok(groups)
}

impl Starry {
    pub(crate) async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        let environment = RequestEnvironment::capture()?;
        let groups = discover_board_test_groups(
            self.app.workspace_root(),
            &args.test_case,
            args.board.as_deref(),
        )?;
        if args.list {
            let case_names = board_test::labeled_board_cases(groups);
            println!(
                "{}",
                qemu_test::render_labeled_case_forest("starry", [("board", case_names)])
            );
            return Ok(());
        }

        let mut run_state = board_test::BoardTestRunState::new("starry", groups.len());
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
            let missing_env = board_test::missing_required_env(&group.required_env);
            if !missing_env.is_empty() {
                run_state.skip_group(group_label, &missing_env);
                continue;
            }

            runnable.push((index, group_label, group));
        }

        let mut build_groups = BTreeMap::<PathBuf, Vec<usize>>::new();
        let mut resolved_build_configs = HashMap::<usize, PathBuf>::new();
        let mut build_config_errors = HashMap::<usize, String>::new();
        for (position, (_, _, group)) in runnable.iter().enumerate() {
            match group.build_config_path.canonicalize() {
                Ok(build_config) => {
                    resolved_build_configs.insert(position, build_config.clone());
                    build_groups.entry(build_config).or_default().push(position);
                }
                Err(error) => {
                    build_config_errors.insert(
                        position,
                        format!(
                            "failed to resolve Starry board build config `{}`: {error}",
                            group.build_config_path.display()
                        ),
                    );
                }
            }
        }

        // Board TOMLs only select runtime checks and session files. Build each
        // unique kernel configuration once, preserving its ELF before another
        // configuration can overwrite Cargo's common artifact path.
        let artifact_parent = self.app.target_dir().join("axbuild");
        std::fs::create_dir_all(&artifact_parent).with_context(|| {
            format!(
                "failed to create Starry board artifact parent {}",
                artifact_parent.display()
            )
        })?;
        let artifact_directory = tempfile::Builder::new()
            .prefix("starry-board-artifacts-")
            .tempdir_in(&artifact_parent)
            .context("failed to create temporary Starry board artifact directory")?;
        let mut prepared_builds = Vec::<PreparedBoardBuildGroup>::new();
        let mut config_to_build = HashMap::<PathBuf, usize>::new();
        for (build_config, positions) in build_groups {
            let group = &runnable[positions[0]].2;
            let prepared = async {
                let request = self.prepare_request_from_input(
                    Self::test_board_build_args(group),
                    group.build_input.clone(),
                    environment.clone(),
                    None,
                    None,
                    SnapshotPersistence::Discard,
                )?;
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

            let result = async {
                let output = self.build_artifact(&request, cargo.clone()).await?;
                let elf_path = qemu_test::preserve_build_artifact(
                    output.elf_path(),
                    artifact_directory.path(),
                    prepared_builds.len(),
                )?;
                Ok::<_, anyhow::Error>(PreparedBoardBuild {
                    cargo: cargo.clone(),
                    elf_path,
                })
            }
            .await;
            let index = prepared_builds.len();
            prepared_builds.push(PreparedBoardBuildGroup {
                cargo,
                result: result.map_err(|error| format!("{error:#}")),
            });
            config_to_build.insert(build_config, index);
        }

        let mut batches = Vec::new();
        for (position, (_, group_label, group)) in runnable.into_iter().enumerate() {
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
                        group.build_config_path.display()
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
                let (config, config_path) = self
                    .load_board_config(&prepared.cargo, Some(board_test_config.as_path()))
                    .await?;
                let case_dir = config_path.parent().context("board config has no parent")?;
                let assets = prepare_board_session_assets(BoardSessionAssetRequest {
                    workspace_root: self.app.workspace_root(),
                    target_dir: self.app.target_dir(),
                    arch: &group.arch,
                    target: &group.target,
                    case_name: &group.name,
                    case_dir,
                    board_config_path: &config_path,
                    declared_session_files: &config.session_files,
                })
                .await?;
                Ok::<_, anyhow::Error>(super::board_batch::PreparedBoardCase {
                    label: group_label.clone(),
                    group,
                    config,
                    config_path,
                    assets,
                })
            }
            .await;
            match result {
                Ok(case) => super::board_batch::append_case(&mut batches, *build_index, case),
                Err(error) => run_state.fail_group(
                    group_label,
                    error.context(format!(
                        "failed to prepare board case {board_test_config_summary}"
                    )),
                ),
            }
        }

        let batch_count = batches.len();
        for (index, batch) in batches.into_iter().enumerate() {
            let labels = batch
                .members
                .iter()
                .map(|member| member.label.clone())
                .collect::<Vec<_>>();
            println!(
                "[axbuild] board boot {}/{batch_count}: {} member(s): {}",
                index + 1,
                batch.members.len(),
                labels.join(", ")
            );
            let prepared = prepared_builds[batch.build_index]
                .result
                .as_ref()
                .map_err(|message| anyhow::anyhow!("{message}"))?;
            let result = async {
                let first = batch.members.first().context("empty board batch")?;
                let mut config = super::board_batch::combined_config(&batch)?;
                let _boot_entropy =
                    crate::starry::boot_entropy::prepare_for_secure_wifi(&mut config)?;
                let options = RunBoardOptions {
                    board_type: args.board_type.clone(),
                    server: args.server.clone(),
                    port: args.port,
                };
                let board_request = if batch.members.len() == 1 {
                    match &first.assets {
                        Some(assets) => BoardRunRequest::new(config, options)
                            .with_session_files(&assets.root, &assets.relative_paths)?,
                        None => board_run_request(&first.config_path, config, options)?,
                    }
                } else {
                    let upload_root = artifact_directory.path().join(format!("upload-{index}"));
                    std::fs::create_dir(&upload_root)?;
                    let paths = super::board_batch::prepare_uploads(&batch, &upload_root)?;
                    BoardRunRequest::new(config, options)
                        .with_session_files(&upload_root, &paths)?
                };
                self.app
                    .board_prepared_elf_with_request(
                        prepared.elf_path.clone(),
                        Some(first.group.build_config_path.clone()),
                        board_request,
                    )
                    .await
            }
            .await;
            match result {
                Ok(()) => {
                    for label in labels {
                        run_state.pass_group(&label);
                    }
                }
                Err(error) => {
                    // A failed boot proves no complete batch result. Never report
                    // unobserved later members as passed.
                    for label in labels {
                        run_state.fail_group(
                            label,
                            anyhow::anyhow!("shared board boot failed: {error:#}"),
                        );
                    }
                }
            }
        }
        run_state.finish()
    }

    pub(crate) fn test_build_args(target: &str, config: Option<PathBuf>) -> StarryCliArgs {
        StarryCliArgs {
            preset: None,
            config,
            arch: None,
            target: Some(target.to_string()),
            smp: None,
            debug: false,
        }
    }

    fn test_board_build_args(group: &StarryBoardTestGroup) -> StarryCliArgs {
        StarryCliArgs {
            preset: None,
            config: None,
            arch: Some(group.arch.clone()),
            target: Some(group.target.clone()),
            smp: None,
            debug: false,
        }
    }
}
