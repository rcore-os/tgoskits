//! Application selection, preparation and run orchestration.
use ostool::board::RunBoardOptions;

use crate::{
    context::{RequestEnvironment, SnapshotPersistence, StarryCliArgs, arch_for_target_checked},
    execution::{case as qemu_case, host_http::HostHttpServerGuard, qemu},
    starry::{Starry, app, build, rootfs, test},
};

impl Starry {
    pub(crate) async fn app_command(&mut self, args: app::ArgsApp) -> anyhow::Result<()> {
        match args.command {
            app::AppCommand::List(args) => app::print_apps(self.app.workspace_root(), args.kind),
            app::AppCommand::Qemu(args) => self.app_qemu_run(args).await,
            app::AppCommand::Board(args) => self.app_board(args).await,
        }
    }

    async fn app_qemu_run(&mut self, args: app::ArgsAppQemu) -> anyhow::Result<()> {
        if (args.nixos_case.is_some() || args.all_nixos_cases || args.list_nixos_cases)
            && args.test_case.as_deref() != Some("nixos")
        {
            anyhow::bail!("NixOS case options require `-t nixos`");
        }
        if args.test_case.as_deref() == Some("nixos") {
            let workspace = self.app.workspace_root().to_path_buf();
            return app::run_nixos_app(&workspace, &args, async |request| {
                test::run_nixos(self, request).await
            })
            .await;
        }
        let apps = app::selected_apps(self.app.workspace_root(), &args, app::StarryAppKind::Qemu)?;
        let mut summary = crate::execution::qemu::QemuTestSummary::default();
        let mut skipped = 0;
        for (index, app) in apps.iter().enumerate() {
            let missing = app::missing_caps(app, &args.caps);
            if !missing.is_empty() {
                if args.test_case.is_some() {
                    anyhow::bail!(
                        "Starry app `{}` is missing required capabilities: {}",
                        app.name,
                        missing.join(", ")
                    );
                }
                println!("SKIP\t{}\tmissing {}", app.name, missing.join(","));
                skipped += 1;
                continue;
            }
            println!(
                "{}",
                format_app_run_progress(index + 1, apps.len(), &app.name, args.arch.as_deref())
            );
            let result = self.app_qemu(app, &args).await;
            if !args.all {
                return result;
            }
            match result {
                Ok(()) => summary.pass_with_detail(
                    &app.name,
                    args.arch
                        .as_deref()
                        .unwrap_or(crate::context::DEFAULT_STARRY_ARCH),
                ),
                Err(error) => {
                    eprintln!("failed: {}: {error:#}", app.name);
                    summary.fail_with_detail(&app.name, format!("{error:#}"));
                }
            }
        }
        anyhow::ensure!(apps.len() > skipped, "no Starry QEMU apps ran");
        summary.finish_with_total_detail(
            "Starry apps",
            "app",
            Some(&format!("selected={} skipped={skipped}", apps.len())),
        )
    }

    async fn app_qemu(
        &mut self,
        app: &app::StarryAppCase,
        args: &app::ArgsAppQemu,
    ) -> anyhow::Result<()> {
        let environment = RequestEnvironment::capture()?;
        let case = app::prepare_qemu_app_case(
            self.app.workspace_root(),
            self.app.target_dir(),
            app,
            args.arch.as_deref(),
            args.qemu_config.as_deref(),
        )
        .await?;
        let build_input = match case.build_input.clone() {
            Some(input) => input,
            None => crate::build::load_config_input(self.app.workspace_root(), None, None)?,
        };
        let request = self.prepare_request_from_input(
            StarryCliArgs {
                preset: None,
                config: None,
                arch: Some(case.arch.clone()),
                target: Some(case.target.clone()),
                smp: None,
                debug: args.debug,
            },
            build_input,
            environment,
            case.qemu_config_path.clone(),
            None,
            SnapshotPersistence::Store,
        )?;

        let Some(test_case) = app::app_qemu_test_case(&case, app.case_dir.clone()) else {
            return rootfs::qemu_with_explicit_rootfs(
                self,
                request,
                case.rootfs_path.clone(),
                case.rootfs_write_policy,
            )
            .await;
        };
        if app.prebuild_path.is_some()
            && test_case.test_commands.is_empty()
            && test_case.subcases.is_empty()
        {
            let rootfs_path = crate::image::storage::resolve_explicit_rootfs(
                self.app.workspace_root(),
                self.app.target_dir(),
                &request.arch,
                case.rootfs_path.clone(),
            )?;
            rootfs::ensure_qemu_rootfs_ready(
                &request,
                self.app.workspace_root(),
                self.app.target_dir(),
                Some(&rootfs_path),
            )
            .await?;
            self.app.set_debug_mode(request.debug)?;
            let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
            let mut qemu = self
                .app
                .read_qemu_config_from_path_for_cargo(&cargo, &test_case.qemu_config_path)
                .await?;
            rootfs::patch_rootfs(
                &mut qemu,
                &rootfs_path,
                rootfs::RootfsPatchOptions {
                    mode: rootfs::RootfsPatchMode::EnsureDiskBootNet,
                    write_policy: case.rootfs_write_policy,
                },
            )?;
            qemu::apply_timeout_scale(&mut qemu);
            println!("  prepare assets: 0ns (pipeline=plain, cache=miss)");
            println!(
                "  qemu config: {} (timeout={})",
                test_case.qemu_config_path.display(),
                qemu::qemu_timeout_summary(&qemu)
            );
            println!("  rootfs: {}", rootfs_path.display());
            // Start a [host_http_server] (if configured) before booting; keep the
            // guard alive across the run so e.g. an online pip/uv install can hit
            // a local wheel index at 10.0.2.2:PORT over real TCP.
            let _host_http_server = test_case
                .host_http_server
                .as_ref()
                .map(|config| {
                    crate::execution::host_http::HostHttpServerGuard::start(config, &test_case.name)
                })
                .transpose()?;
            return self.run_qemu_artifact(&request, cargo, qemu).await;
        }
        let rootfs_path = crate::image::storage::resolve_explicit_rootfs(
            self.app.workspace_root(),
            self.app.target_dir(),
            &request.arch,
            case.rootfs_path.clone(),
        )?;
        rootfs::ensure_qemu_rootfs_ready(
            &request,
            self.app.workspace_root(),
            self.app.target_dir(),
            Some(&rootfs_path),
        )
        .await?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        let asset_config = test::starry_case_asset_config();
        let mut qemu = self
            .app
            .read_qemu_config_from_path_for_cargo(&cargo, &test_case.qemu_config_path)
            .await?;
        qemu_case::apply_grouped_qemu_config(
            &mut qemu,
            &test_case,
            &asset_config.grouped_execution,
        );
        let prepare_started = std::time::Instant::now();
        let prepared_assets = qemu_case::prepare_case_assets(
            self.app.target_dir(),
            &request.arch,
            &request.target,
            &test_case,
            rootfs_path,
            asset_config,
        )
        .await?;
        rootfs::patch_rootfs(
            &mut qemu,
            &prepared_assets.rootfs_path,
            rootfs::RootfsPatchOptions {
                mode: rootfs::RootfsPatchMode::EnsureDiskBootNet,
                write_policy: case.rootfs_write_policy,
            },
        )?;
        qemu::apply_timeout_scale(&mut qemu);
        println!(
            "  prepare assets: {:.2?} (pipeline={}, cache={})",
            prepare_started.elapsed(),
            prepared_assets.pipeline.as_str(),
            if prepared_assets.cache_hit {
                "hit"
            } else {
                "miss"
            }
        );
        println!(
            "  qemu config: {} (timeout={})",
            test_case.qemu_config_path.display(),
            qemu::qemu_timeout_summary(&qemu)
        );
        println!("  rootfs: {}", prepared_assets.rootfs_path.display());

        let _host_http_server = test_case
            .host_http_server
            .as_ref()
            .map(|config| HostHttpServerGuard::start(config, &test_case.name))
            .transpose()?;

        let result = self.run_qemu_artifact(&request, cargo, qemu).await;
        drop(prepared_assets);
        result
    }

    async fn app_board(&mut self, args: app::ArgsAppBoard) -> anyhow::Result<()> {
        let environment = RequestEnvironment::capture()?;
        let case = app::resolve_board_case(
            self.app.workspace_root(),
            &args.test_case,
            args.board_config.as_deref(),
        )?;
        let request = self.prepare_request_from_input(
            StarryCliArgs {
                preset: None,
                config: None,
                arch: Some(arch_for_target_checked(&case.target)?.to_owned()),
                target: Some(case.target.clone()),
                smp: None,
                debug: args.debug,
            },
            case.build_input.clone(),
            environment,
            None,
            None,
            SnapshotPersistence::Store,
        )?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        let (mut board_config, board_config_path) = self
            .load_board_config(&cargo, Some(case.board_config_path.as_path()))
            .await?;
        app::configure_board_init_step(&mut board_config, &case.init_cmd)?;
        let arch = arch_for_target_checked(&case.target)?;
        let session_assets = app::prepare_app_board_session_assets(
            self.app.workspace_root(),
            self.app.target_dir(),
            arch,
            &case.target,
            &case,
            &board_config.session_files,
        )
        .await?;
        if args.linux_stage {
            let assets = session_assets.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Starry app `{}` has no Rust or declared session files to stage",
                    args.test_case
                )
            })?;
            return app::stage_in_default_linux(&args, &board_config, assets).await;
        }
        self.run_board_artifact(
            &request,
            cargo,
            board_config,
            board_config_path,
            session_assets,
            RunBoardOptions {
                board_type: args.board_type,
                server: args.server,
                port: args.port,
            },
        )
        .await
    }
}

fn format_app_run_progress(
    index: usize,
    total: usize,
    app_name: &str,
    arch: Option<&str>,
) -> String {
    match arch {
        Some(arch) => format!("RUN\t{index}/{total}\t{app_name}\tarch={arch}"),
        None => format!("RUN\t{index}/{total}\t{app_name}"),
    }
}
