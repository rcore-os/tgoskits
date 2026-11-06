use super::*;

pub(super) async fn run_board(args: ArgsKtestBoard) -> anyhow::Result<()> {
    let mut app = AppContext::new()?;
    let discovered = load_discovered_ktest_package(app.workspace_metadata(), &args.package)?;
    if !discovered.uses_workspace_axtest {
        bail!(
            "package `{}` must declare workspace `axtest` directly as a dependency",
            args.package
        );
    }
    let package = KtestPackage {
        name: discovered.name.clone(),
        targets: discovered.targets.clone(),
    };
    let target = select_ktest_target(&package, Some(&args.test))?;
    let runtime = discovered.runtime;
    let package_dir = discovered.manifest_path.parent().ok_or_else(|| {
        anyhow!(
            "invalid manifest path {}",
            discovered.manifest_path.display()
        )
    })?;
    let board = args.board;
    let selected_config = match args.config.as_ref() {
        Some(path) => Some(path.clone()),
        None if args.preset.is_none() => default_board_build_config(package_dir, &board)?,
        None => None,
    };
    let selected_preset = args
        .preset
        .as_deref()
        .or_else(|| selected_config.is_none().then_some(board.as_str()));
    let board_config_path = args.board_config.unwrap_or_else(|| {
        default_board_run_config(app.workspace_root(), package_dir, runtime, &board)
    });
    let board_config_path = app
        .workspace_root()
        .join(board_config_path)
        .canonicalize()?;
    let parsed_config = KtestBuildConfig::load(
        app.workspace_root(),
        KtestBuildSelection {
            runtime,
            config: selected_config.as_deref(),
            preset: selected_preset,
            default_qemu_target: None,
        },
    )?;
    let build_config = parsed_config.config_path().map(Path::to_path_buf);
    let triple = parsed_config
        .target()
        .ok_or_else(|| anyhow!("board ktest requires a target in its selected build input"))?
        .to_string();
    let arch = crate::context::arch_for_target_checked(&triple)?;
    let mut cargo = parsed_config.cargo(
        app.workspace_context(),
        &KtestUnitConfig {
            package: &args.package,
            arch,
            target: &triple,
            run_config: &board_config_path,
            config: &parsed_config,
        },
        false,
    )?;
    prepare_ktest_cargo(&mut cargo, target, runtime, false);

    crate::axvisor::test::prepare_guest_builds(&mut app, &board_config_path).await?;

    let mut board_config = app
        .read_board_run_config_from_path_for_cargo(&cargo, &board_config_path)
        .await
        .with_context(|| {
            format!(
                "failed to read board config {}",
                board_config_path.display()
            )
        })?;
    apply_axtest_board_markers(&mut board_config);
    let report_session =
        maybe_start_starry_future_incompat_report(runtime, app.target_dir(), arch)?;
    let build_result = app.build(cargo.clone(), build_config.clone()).await;
    let output = crate::build::finish_future_incompat_report_session(report_session, build_result)?;
    maybe_postprocess_starry_artifact(
        KtestBuildContext {
            runtime,
            workspace_root: app.workspace_root(),
            package: &args.package,
            arch,
            target: &triple,
            config: &parsed_config,
        },
        &cargo,
        &output,
    )?;
    if runtime == KtestRuntime::Axvisor {
        prepare_axvisor_ktest_payload(
            &mut app,
            &cargo,
            &KtestUnitConfig {
                package: &args.package,
                arch,
                target: &triple,
                run_config: &board_config_path,
                config: &parsed_config,
            },
            &mut board_config.boot,
            true,
        )
        .await?;
    }
    app.board_prepared_elf(
        output.elf_path().to_path_buf(),
        build_config,
        board_config,
        RunBoardOptions {
            board_type: args.board_type,
            server: args.server,
            port: args.port,
        },
    )
    .await
}

