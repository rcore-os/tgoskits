use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context, bail};
use ostool::{build::config::Cargo, run::qemu::QemuConfig};

use super::{
    ArgsTestQemu, PreparedStarryQemuCase, StarryQemuCase, StarryQemuCaseOutcome,
    StarryQemuCaseReport, StarryQemuCaseRequirements, StarryQemuRunReport,
    discover_all_qemu_cases_with_archs, discover_qemu_cases, ensure_host_symbolize_output_matches,
    finalize_qemu_case_run, parse_test_target, starry_case_asset_config,
    start_qemu_case_host_http_server,
};
use crate::{
    build::{append_encoded_rustflags, env_truthy},
    context::{ResolvedStarryRequest, SnapshotPersistence},
    starry::{Starry, board, build, rootfs},
    test::{case, qemu as qemu_test, timing},
};

const AXTEST_RUSTFLAGS: &[&str] = &["--cfg", "axtest", "--check-cfg", "cfg(axtest)"];

impl Starry {
    pub(super) async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        if args.list && args.arch.is_none() && args.target.is_none() {
            let case_names = discover_all_qemu_cases_with_archs(
                self.app.workspace_root(),
                args.test_case.as_deref(),
            )
            .map_err(anyhow::Error::new)?;
            println!(
                "{}",
                qemu_test::render_qemu_case_forest("starry", [("qemu", case_names)])
            );
            return Ok(());
        }

        let (arch, target) =
            parse_test_target(self.app.workspace_root(), &args.arch, &args.target)?;
        let cases = discover_qemu_cases(
            self.app.workspace_root(),
            &arch,
            &target,
            args.test_case.as_deref(),
        )?;
        if args.list {
            let case_names = cases.iter().map(|case| case.case.display_name.as_str());
            println!("{}", qemu_test::render_case_tree("qemu", case_names));
            return Ok(());
        }
        let package = crate::context::STARRY_PACKAGE;

        println!(
            "running starry qemu tests for package {} on arch: {} (target: {})",
            package, arch, target
        );

        let default_board = board::default_board_for_target(self.app.workspace_root(), &target)?;
        let request = self.prepare_request(
            Self::test_build_args(&target, None),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let mut request = Self::qemu_test_request(request);
        if let Some(default_board) = default_board {
            request.build_info_override = Some(default_board.build_info);
        } else {
            anyhow::bail!(
                "missing Starry qemu defconfig for target `{target}` in tests; expected a default \
                 qemu board config under os/StarryOS/configs/board"
            );
        }
        let default_rootfs_path =
            crate::image::storage::default_rootfs_path(self.app.workspace_root(), &request.arch)?;
        self.app.set_debug_mode(request.debug)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut reports = Vec::new();
        let asset_config = starry_case_asset_config();

        let timing_stage = timing::TimingStage::new(
            "starry-qemu",
            [
                ("phase", "prepare-build-groups".to_string()),
                ("arch", arch.clone()),
                ("target", target.clone()),
            ],
        );
        let build_groups = qemu_test::prepare_case_build_groups(&cases, |build_config_path| {
            Self::qemu_group_build_context(&request, build_config_path)
        });
        timing_stage.finish();
        let build_groups = build_groups?;

        let mut completed = 0;
        for build_group in &build_groups {
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("build_group", build_group.group.build_group.to_string()),
                    ("phase", "build".to_string()),
                ],
            );
            let build_result = self
                .build_artifact(&build_group.request, build_group.cargo.clone())
                .await;
            timing_stage.finish();
            build_result.with_context(|| {
                format!(
                    "failed to build Starry qemu test artifact for build group `{}` ({})",
                    build_group.group.build_group,
                    build_group.group.build_config_path.display()
                )
            })?;

            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("build_group", build_group.group.build_group.to_string()),
                    ("phase", "prepare-qemu-cases".to_string()),
                ],
            );
            let cases = self
                .prepare_qemu_cases(
                    &build_group.request,
                    &build_group.cargo,
                    &default_rootfs_path,
                    &build_group.group.cases,
                )
                .await;
            timing_stage.finish();
            let cases = cases.with_context(|| {
                format!(
                    "failed to load Starry qemu test cases for build group `{}` ({})",
                    build_group.group.build_group,
                    build_group.group.build_config_path.display()
                )
            })?;

            for case in &cases {
                completed += 1;
                let case_name = &case.case.name;
                println!("[{completed}/{total}] starry qemu {case_name}");

                let case_started = Instant::now();
                match self
                    .run_qemu_case(
                        &build_group.request,
                        &build_group.cargo,
                        case,
                        &asset_config,
                    )
                    .await
                    .with_context(|| format!("starry qemu test failed for case `{case_name}`"))
                {
                    Ok(()) => {
                        println!("ok: {case_name}");
                        reports.push(StarryQemuCaseReport {
                            name: case_name.clone(),
                            outcome: StarryQemuCaseOutcome::Passed,
                            duration: case_started.elapsed(),
                        });
                    }
                    Err(err) => {
                        eprintln!("failed: {case_name}: {err:#}");
                        reports.push(StarryQemuCaseReport {
                            name: case_name.clone(),
                            outcome: StarryQemuCaseOutcome::Failed,
                            duration: case_started.elapsed(),
                        });
                        finalize_qemu_case_run(&StarryQemuRunReport {
                            cases: reports,
                            total_duration: suite_started.elapsed(),
                        })?;
                        bail!("starry qemu test aborted: case `{case_name}` failed");
                    }
                }
            }
        }

        finalize_qemu_case_run(&StarryQemuRunReport {
            cases: reports,
            total_duration: suite_started.elapsed(),
        })
    }

    async fn prepare_qemu_cases(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
        default_rootfs_path: &Path,
        cases: &[&StarryQemuCase],
    ) -> anyhow::Result<Vec<PreparedStarryQemuCase>> {
        let mut prepared = Vec::with_capacity(cases.len());
        let mut rootfs_paths = BTreeSet::new();
        for starry_case in cases {
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("case", starry_case.case.display_name.clone()),
                    ("phase", "read-qemu-config".to_string()),
                ],
            );
            let qemu_result = self
                .app
                .read_qemu_config_from_path_for_cargo(cargo, &starry_case.case.qemu_config_path)
                .await;
            timing_stage.finish();
            let mut qemu = qemu_result.with_context(|| {
                format!(
                    "failed to read Starry qemu config for case `{}`",
                    starry_case.case.display_name
                )
            })?;
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("case", starry_case.case.display_name.clone()),
                    ("phase", "prepare-qemu-config".to_string()),
                ],
            );
            Self::rewrite_qemu_case_managed_rootfs_paths(self.app.workspace_root(), &mut qemu)?;
            let rootfs_path =
                Self::qemu_case_rootfs_path(self.app.workspace_root(), &qemu, default_rootfs_path)?;
            rootfs_paths.insert(rootfs_path.clone());
            rootfs_paths.extend(Self::qemu_case_managed_rootfs_paths(
                self.app.workspace_root(),
                &qemu,
            )?);
            qemu_test::validate_grouped_qemu_commands(&qemu, &starry_case.case, "Starry")?;
            let requirements = Self::qemu_case_requirements(&qemu).with_context(|| {
                format!(
                    "failed to read QEMU requirements for `{}`",
                    starry_case.case.display_name
                )
            })?;
            timing_stage.finish();
            prepared.push(PreparedStarryQemuCase {
                case: starry_case.case.clone(),
                qemu,
                build_group: starry_case.build_group.clone(),
                build_config_path: starry_case.build_config_path.clone(),
                rootfs_path,
                requirements,
            });
        }

        let timing_stage = timing::TimingStage::new(
            "starry-qemu",
            [
                ("phase", "ensure-rootfs-paths".to_string()),
                ("rootfs_count", rootfs_paths.len().to_string()),
            ],
        );
        let ensure_result = self
            .ensure_qemu_case_rootfs_paths(request, default_rootfs_path, &rootfs_paths)
            .await;
        timing_stage.finish();
        ensure_result?;
        Ok(prepared)
    }

    async fn ensure_qemu_case_rootfs_paths(
        &self,
        request: &ResolvedStarryRequest,
        default_rootfs_path: &Path,
        rootfs_paths: &BTreeSet<PathBuf>,
    ) -> anyhow::Result<()> {
        for rootfs_path in rootfs_paths {
            let rootfs_kind = if rootfs_path == default_rootfs_path {
                "default"
            } else {
                "managed"
            };
            let timing_stage = timing::TimingStage::new(
                "starry-qemu",
                [
                    ("phase", "ensure-rootfs-path".to_string()),
                    ("rootfs_kind", rootfs_kind.to_string()),
                    ("rootfs", rootfs_path.display().to_string()),
                ],
            );
            let result = if rootfs_path == default_rootfs_path {
                rootfs::ensure_rootfs_in_tmp_dir(
                    self.app.workspace_root(),
                    &request.arch,
                    &request.target,
                )
                .await
                .map(|_| ())
            } else {
                crate::image::storage::ensure_optional_managed_rootfs(
                    self.app.workspace_root(),
                    &request.arch,
                    Some(rootfs_path),
                )
                .await
            };
            timing_stage.finish();
            result?;
        }
        Ok(())
    }

    pub(crate) fn qemu_case_rootfs_path(
        workspace_root: &Path,
        qemu: &QemuConfig,
        default_rootfs_path: &Path,
    ) -> anyhow::Result<PathBuf> {
        Ok(Self::qemu_case_managed_rootfs_paths(workspace_root, qemu)?
            .into_iter()
            .next()
            .unwrap_or_else(|| default_rootfs_path.to_path_buf()))
    }

    pub(crate) fn qemu_case_managed_rootfs_paths(
        workspace_root: &Path,
        qemu: &QemuConfig,
    ) -> anyhow::Result<Vec<PathBuf>> {
        crate::rootfs::qemu::drive_file_paths(qemu)
            .into_iter()
            .filter_map(|path| {
                crate::image::storage::resolve_managed_rootfs_path(workspace_root, &path)
                    .transpose()
            })
            .collect()
    }

    pub(crate) fn rewrite_qemu_case_managed_rootfs_paths(
        workspace_root: &Path,
        qemu: &mut QemuConfig,
    ) -> anyhow::Result<()> {
        crate::rootfs::qemu::rewrite_drive_file_paths(qemu, |path| {
            crate::image::storage::resolve_managed_rootfs_path(workspace_root, path)
        })
    }

    pub(crate) fn qemu_case_requirements(
        qemu: &QemuConfig,
    ) -> anyhow::Result<StarryQemuCaseRequirements> {
        Ok(StarryQemuCaseRequirements {
            smp: qemu_test::smp_from_qemu_arg(qemu).unwrap_or(1),
        })
    }

    pub(crate) fn qemu_group_build_context(
        request: &ResolvedStarryRequest,
        build_config_path: &Path,
    ) -> anyhow::Result<(ResolvedStarryRequest, Cargo)> {
        let request = Self::request_for_qemu_case_build_config(request, build_config_path);
        let mut cargo = build::load_cargo_config(&request)?;
        if env_truthy(&cargo.env, "AXTEST") {
            append_encoded_rustflags(&mut cargo, AXTEST_RUSTFLAGS);
        }
        if crate::support::axtest_coverage::enabled(&cargo) {
            crate::support::axtest_coverage::prepare_cargo(&mut cargo);
        }

        Ok((request, cargo))
    }

    fn request_for_qemu_case_build_config(
        request: &ResolvedStarryRequest,
        build_config_path: &Path,
    ) -> ResolvedStarryRequest {
        let mut request = request.clone();
        request.build_info_path = build_config_path.to_path_buf();
        request.build_info_override = None;
        request
    }

    pub(crate) fn qemu_test_request(mut request: ResolvedStarryRequest) -> ResolvedStarryRequest {
        request.smp = None;
        request
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
        prepared_case: &PreparedStarryQemuCase,
        asset_config: &case::CaseAssetConfig,
    ) -> anyhow::Result<()> {
        let case = &prepared_case.case;
        let mut qemu = prepared_case.qemu.clone();
        case::apply_grouped_qemu_config(&mut qemu, case, &asset_config.grouped_runner);

        qemu_test::apply_smp_qemu_arg(&mut qemu, Some(prepared_case.requirements.smp));
        qemu_test::apply_timeout_scale(&mut qemu);

        let case_name = &case.name;
        let auto_symbolize =
            crate::build::build_info_enables_backtrace_path(&prepared_case.build_config_path);
        if !case.host_symbolize_success_regex.is_empty() && !auto_symbolize {
            bail!(
                "Starry qemu case `{case_name}` requests host symbolize assertions but its build \
                 config does not enable BACKTRACE=y or DWARF=y"
            );
        }

        let keep_qemu_log = crate::backtrace::keep_qemu_log_from_env();
        let elf = crate::backtrace::std_test_elf_path(
            self.app.workspace_root(),
            &request.target,
            crate::context::STARRY_PACKAGE,
            request.debug,
        );
        let stream_session = if auto_symbolize {
            crate::backtrace::BacktraceSymbolizeSession::try_new(&elf, case_name)
        } else {
            None
        };
        let capture_backtrace = if auto_symbolize {
            let dir = crate::context::axbuild_tmp_dir(self.app.workspace_root()).join("qemu-logs");
            fs::create_dir_all(&dir)?;
            Some(crate::backtrace::BacktraceQemuCapture {
                log_path: dir.join(format!("starry-{case_name}-{}.log", request.target)),
                stream_symbolize: stream_session.clone(),
                suppress_terminal_raw_blocks: false,
                write_log_during_capture: keep_qemu_log,
                captured_blocks: Arc::new(std::sync::Mutex::new(Vec::new())),
            })
        } else {
            None
        };
        let log_path = capture_backtrace
            .as_ref()
            .map(|capture| capture.log_path.clone());
        let memory_blocks = capture_backtrace
            .as_ref()
            .map(|capture| capture.captured_blocks.clone());

        let prepare_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "prepare-assets".to_string()),
            ],
        );
        let prepared_assets_result = case::prepare_case_assets(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
            case,
            prepared_case.rootfs_path.clone(),
            asset_config.clone(),
        )
        .await;
        if prepared_assets_result.is_err() {
            timing::print_timing_line(
                "qemu-case",
                &[
                    ("case", case.display_name.clone()),
                    ("phase", "prepare-assets".to_string()),
                    ("status", "failed".to_string()),
                ],
                prepare_stage.elapsed(),
            );
        }
        let prepared_assets = prepared_assets_result?;
        let prepare_elapsed = prepare_stage.elapsed();
        timing::print_timing_line(
            "qemu-case",
            &[
                ("case", case.display_name.clone()),
                ("phase", "prepare-assets".to_string()),
                ("pipeline", prepared_assets.pipeline.as_str().to_string()),
                (
                    "cache",
                    if prepared_assets.cache_hit {
                        "hit"
                    } else {
                        "miss"
                    }
                    .to_string(),
                ),
            ],
            prepare_elapsed,
        );
        let timing_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "patch-rootfs".to_string()),
            ],
        );
        rootfs::patch_rootfs(
            &mut qemu,
            &prepared_assets.rootfs_path,
            rootfs::RootfsPatchMode::EnsureDiskBootNet,
        );
        timing_stage.finish();
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());
        let timing_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "apply-dynamic-boot".to_string()),
            ],
        );
        timing_stage.finish();
        let timing_stage = timing::TimingStage::new(
            "qemu-case",
            [
                ("case", case.display_name.clone()),
                ("phase", "start-host-http".to_string()),
            ],
        );
        let host_http_result = start_qemu_case_host_http_server(case);
        timing_stage.finish();
        let _host_http_server = host_http_result?;
        case::run_qemu_with_prepared_case_assets(
            &mut self.app,
            cargo,
            qemu,
            capture_backtrace,
            &case.qemu_config_path,
            prepared_assets,
            case::RunPreparedQemuCaseOptions {
                prepare_elapsed,
                qemu_timing_fields: Some(vec![("case", case.display_name.clone())]),
            },
        )
        .await?;

        if auto_symbolize && let Some(path) = log_path {
            let blocks_snapshot = memory_blocks.and_then(|arc| arc.lock().ok().map(|b| b.clone()));
            let symbolized_output = if !case.host_symbolize_success_regex.is_empty() {
                match blocks_snapshot.as_deref() {
                    Some(blocks) => crate::backtrace::symbolize_captured_blocks_to_string(
                        &elf, case_name, blocks,
                    )?,
                    None => None,
                }
            } else {
                None
            };
            let blocks_ref = blocks_snapshot.as_deref();
            let outcome = crate::backtrace::maybe_symbolize_after_qemu(
                &elf,
                &path,
                case_name,
                keep_qemu_log,
                stream_session.as_deref(),
                blocks_ref,
            )?;

            if !case.host_symbolize_success_regex.is_empty() {
                ensure_host_symbolize_output_matches(
                    case_name,
                    outcome,
                    symbolized_output.as_deref(),
                    &case.host_symbolize_success_regex,
                )?;
            }
        }

        Ok(())
    }
}
