use std::path::{Path, PathBuf};

use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
};

use crate::{
    context::{AppContext, ResolvedStarryRequest, SnapshotPersistence, StarryCliArgs},
    test::{case as qemu_case, host_http::HostHttpServerGuard, qemu},
};

pub(crate) mod apk;
pub mod app;
mod args;
pub mod board;
pub mod build;
pub mod config;
pub mod kmod;
pub mod perf;
pub mod quick_start;
pub(crate) mod resolver;
pub mod rootfs;
pub mod test;
#[cfg(test)]
mod tests;

pub use args::*;

pub struct Starry {
    pub(super) app: AppContext,
}

impl Starry {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Defconfig(args) => self.defconfig(args),
            Command::Config(args) => self.config(args),
            Command::Perf(args) => self.perf(args).await,
            Command::Rootfs(args) => self.rootfs(args).await,
            Command::QuickStart(args) => self.quick_start(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Board(args) => self.board(args).await,
            Command::Test(args) => self.test(args).await,
            Command::App(args) => self.app_command(args).await,
            Command::Kmod(args) => self.kmod(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args).into(), None, None, SnapshotPersistence::Store)?;
        self.ensure_default_build_config_for_request(&request, "build")?;
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        self.ensure_default_build_config_for_request(&request, "qemu")?;
        if let Some(rootfs) = args.rootfs {
            rootfs::qemu_with_explicit_rootfs(self, request, rootfs).await
        } else {
            self.run_qemu_request(request).await
        }
    }

    async fn rootfs(&mut self, args: rootfs::ArgsRootfs) -> anyhow::Result<()> {
        rootfs::rootfs(self, args).await
    }

    async fn perf(&mut self, args: ArgsPerf) -> anyhow::Result<()> {
        perf::run(self, args).await
    }

    fn defconfig(&mut self, args: ArgsDefconfig) -> anyhow::Result<()> {
        let path = config::write_defconfig(self.app.workspace_root(), &args.board)?;
        println!("Generated {} for board {}", path.display(), args.board);
        Ok(())
    }

    fn config(&mut self, args: ArgsConfig) -> anyhow::Result<()> {
        match args.command {
            ConfigCommand::Ls => {
                for board in config::available_board_names(self.app.workspace_root())? {
                    println!("{board}");
                }
            }
        }
        Ok(())
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let request = self.prepare_request(
            (&args.build).into(),
            None,
            args.uboot_config,
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }

    async fn board(&mut self, args: ArgsBoard) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args.build).into(), None, None, SnapshotPersistence::Store)?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let board_config = self
            .load_board_config(&cargo, args.board_config.as_deref())
            .await?;
        self.run_board_artifact(
            &request,
            cargo,
            board_config,
            RunBoardOptions {
                board_type: args.board_type,
                server: args.server,
                port: args.port,
            },
        )
        .await
    }

    async fn quick_start(&mut self, args: quick_start::ArgsQuickStart) -> anyhow::Result<()> {
        use quick_start::{
            QuickOrangeAction, QuickQemuPlatform, QuickSg2002Action, QuickStartCommand,
        };

        match args.command {
            QuickStartCommand::List => {
                quick_start::print_supported_platforms(self.app.workspace_root());
                Ok(())
            }
            QuickStartCommand::QemuAarch64(args) => {
                self.quick_start_qemu(QuickQemuPlatform::Aarch64, args.action)
                    .await
            }
            QuickStartCommand::QemuRiscv64(args) => {
                self.quick_start_qemu(QuickQemuPlatform::Riscv64, args.action)
                    .await
            }
            QuickStartCommand::QemuLoongarch64(args) => {
                self.quick_start_qemu(QuickQemuPlatform::Loongarch64, args.action)
                    .await
            }
            QuickStartCommand::QemuX8664(args) => {
                self.quick_start_qemu(QuickQemuPlatform::X8664, args.action)
                    .await
            }
            QuickStartCommand::Orangepi5Plus(args) => match args.action {
                QuickOrangeAction::Build(build_args) => {
                    self.quick_start_orangepi_build(build_args).await
                }
                QuickOrangeAction::Run(run_args) => self.quick_start_orangepi_run(run_args).await,
            },
            QuickStartCommand::LicheervNanoSg2002(args) => match args.action {
                QuickSg2002Action::Build => self.quick_start_sg2002_build().await,
                QuickSg2002Action::Run(run_args) => self.quick_start_sg2002_run(run_args).await,
            },
        }
    }

    async fn test(&mut self, args: test::ArgsTest) -> anyhow::Result<()> {
        test::test(self, args).await
    }

    async fn app_command(&mut self, args: app::ArgsApp) -> anyhow::Result<()> {
        match args.command {
            app::AppCommand::List(args) => app::print_apps(self.app.workspace_root(), args.kind),
            app::AppCommand::Qemu(args) => self.app_qemu_run(args).await,
            app::AppCommand::Board(args) => self.app_board(args).await,
        }
    }

    async fn app_qemu_run(&mut self, args: app::ArgsAppQemu) -> anyhow::Result<()> {
        let apps = app::selected_apps(self.app.workspace_root(), &args, app::StarryAppKind::Qemu)?;
        let app_count = apps.len();
        for (index, app) in apps.into_iter().enumerate() {
            let missing = app::missing_caps(&app, &args.caps);
            if !missing.is_empty() {
                if args.test_case.is_some() {
                    anyhow::bail!(
                        "Starry app `{}` is missing required capabilities: {}",
                        app.name,
                        missing.join(", ")
                    );
                }
                println!("SKIP\t{}\tmissing {}", app.name, missing.join(","));
                continue;
            }

            println!(
                "{}",
                format_app_run_progress(index + 1, app_count, &app.name, args.arch.as_deref())
            );
            self.app_qemu(&app, &args).await?;
        }
        Ok(())
    }

    async fn app_qemu(
        &mut self,
        app: &app::StarryAppCase,
        args: &app::ArgsAppQemu,
    ) -> anyhow::Result<()> {
        let case = app::prepare_qemu_app_case(
            self.app.workspace_root(),
            app,
            args.arch.as_deref(),
            args.qemu_config.as_deref(),
        )
        .await?;
        let request = self.prepare_request(
            StarryCliArgs {
                config: case.build_config_path.clone(),
                arch: Some(case.arch.clone()),
                target: Some(case.target.clone()),
                smp: None,
                debug: args.debug,
            },
            case.qemu_config_path.clone(),
            None,
            SnapshotPersistence::Store,
        )?;

        let Some(test_case) = app::app_qemu_test_case(&case, app.case_dir.clone()) else {
            return rootfs::qemu_with_explicit_rootfs(self, request, case.rootfs_path).await;
        };
        if app.prebuild_path.is_some()
            && test_case.test_commands.is_empty()
            && test_case.subcases.is_empty()
        {
            let rootfs_path = crate::image::storage::resolve_explicit_rootfs(
                self.app.workspace_root(),
                &request.arch,
                case.rootfs_path,
            )?;
            rootfs::ensure_qemu_rootfs_ready(
                &request,
                self.app.workspace_root(),
                Some(&rootfs_path),
            )
            .await?;
            self.app.set_debug_mode(request.debug)?;
            let cargo = build::load_cargo_config(&request)?;
            let mut qemu = self
                .app
                .read_qemu_config_from_path_for_cargo(&cargo, &test_case.qemu_config_path)
                .await?;
            rootfs::patch_rootfs(
                &mut qemu,
                &rootfs_path,
                rootfs::RootfsPatchMode::EnsureDiskBootNet,
            );
            if !qemu.args.iter().any(|arg| arg == "-snapshot") {
                qemu.args.push("-snapshot".to_string());
            }
            qemu::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
            if qemu.uefi {
                qemu::apply_drive_snapshot_without_global_snapshot(&mut qemu);
            }
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
                    crate::test::host_http::HostHttpServerGuard::start(config, &test_case.name)
                })
                .transpose()?;
            return self.run_qemu_artifact(&request, cargo, qemu).await;
        }
        let rootfs_path = crate::image::storage::resolve_explicit_rootfs(
            self.app.workspace_root(),
            &request.arch,
            case.rootfs_path,
        )?;
        rootfs::ensure_qemu_rootfs_ready(&request, self.app.workspace_root(), Some(&rootfs_path))
            .await?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let asset_config = test::starry_case_asset_config();
        let mut qemu = self
            .app
            .read_qemu_config_from_path_for_cargo(&cargo, &test_case.qemu_config_path)
            .await?;
        qemu_case::apply_grouped_qemu_config(&mut qemu, &test_case, &asset_config.grouped_runner);
        let prepare_started = std::time::Instant::now();
        let prepared_assets = qemu_case::prepare_case_assets(
            self.app.workspace_root(),
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
            rootfs::RootfsPatchMode::EnsureDiskBootNet,
        );
        qemu.args.extend(prepared_assets.extra_qemu_args.clone());
        qemu::apply_dynamic_platform_qemu_boot(&mut qemu, &cargo);
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
        qemu_case::remove_case_rootfs_copy(prepared_assets.rootfs_copy_to_remove.as_deref());
        qemu_case::remove_case_run_dir(prepared_assets.run_dir_to_remove.as_deref());
        result
    }

    async fn app_board(&mut self, args: app::ArgsAppBoard) -> anyhow::Result<()> {
        let case = app::resolve_board_case(
            self.app.workspace_root(),
            &args.test_case,
            args.board_config.as_deref(),
        )?;
        let request = self.prepare_request(
            StarryCliArgs {
                config: Some(case.build_config_path.clone()),
                arch: None,
                target: Some(case.target.clone()),
                smp: None,
                debug: args.debug,
            },
            None,
            None,
            SnapshotPersistence::Store,
        )?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let mut board_config = self
            .load_board_config(&cargo, Some(case.board_config_path.as_path()))
            .await?;
        if board_config.shell_init_cmd.is_none() {
            board_config.shell_init_cmd = Some(case.init_cmd);
        }
        self.run_board_artifact(
            &request,
            cargo,
            board_config,
            RunBoardOptions {
                board_type: args.board_type,
                server: args.server,
                port: args.port,
            },
        )
        .await
    }

    pub(super) fn prepare_request(
        &self,
        args: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedStarryRequest> {
        let (request, snapshot) = self.app.prepare_starry_request(
            args,
            qemu_config,
            uboot_config,
            build::resolve_build_info_path,
        )?;
        if persistence.should_store() {
            self.app.store_starry_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    pub(super) fn ensure_default_build_config_for_request(
        &self,
        request: &ResolvedStarryRequest,
        command: &str,
    ) -> anyhow::Result<()> {
        if let Some(board) = config::ensure_default_build_config_for_target(
            self.app.workspace_root(),
            &request.target,
            &request.build_info_path,
        )? {
            println!(
                "generated missing Starry {command} build config {} from board {}",
                request.build_info_path.display(),
                board.name
            );
        }
        Ok(())
    }

    fn quick_start_build_args(arch: &str, config: PathBuf) -> StarryCliArgs {
        StarryCliArgs {
            config: Some(config),
            arch: Some(arch.to_string()),
            target: None,
            smp: None,
            debug: false,
        }
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
                .read_uboot_config_from_path_for_cargo(cargo, path)
                .await
                .map(Some),
            None => Ok(None),
        }
    }

    async fn load_board_config(
        &mut self,
        cargo: &Cargo,
        board_config_path: Option<&Path>,
    ) -> anyhow::Result<BoardRunConfig> {
        match board_config_path {
            Some(path) => {
                self.app
                    .read_board_run_config_from_path_for_cargo(cargo, path)
                    .await
            }
            None => {
                let workspace_root = self.app.workspace_root().to_path_buf();
                self.app
                    .ensure_board_run_config_in_dir_for_cargo(cargo, &workspace_root)
                    .await
            }
        }
    }

    pub(super) async fn build_artifact(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: Cargo,
    ) -> anyhow::Result<ostool::build::CargoBuildOutput> {
        build::build_starry_artifact(self, request, cargo).await
    }

    pub(super) async fn run_qemu_artifact(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: Cargo,
        qemu: ostool::run::qemu::QemuConfig,
    ) -> anyhow::Result<()> {
        self.build_artifact(request, cargo.clone()).await?;
        self.app.run_qemu(&cargo, qemu, None).await
    }

    pub(super) async fn run_uboot_artifact(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: Cargo,
        uboot: Option<ostool::run::uboot::UbootConfig>,
    ) -> anyhow::Result<()> {
        let uboot = match uboot {
            Some(uboot) => uboot,
            None => self.app.ensure_uboot_config_for_cargo(&cargo).await?,
        };
        self.build_artifact(request, cargo).await?;
        self.app.run_prepared_uboot(uboot).await
    }

    pub(super) async fn run_board_artifact(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: Cargo,
        board_config: BoardRunConfig,
        options: RunBoardOptions,
    ) -> anyhow::Result<()> {
        let output = self.build_artifact(request, cargo.clone()).await?;
        self.app
            .board_prepared_elf(
                output.elf_path().to_path_buf(),
                cargo.to_bin,
                request.build_info_path.clone(),
                board_config,
                options,
            )
            .await
    }

    async fn run_qemu_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        rootfs::qemu(self, request).await
    }

    async fn run_build_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        self.build_artifact(&request, cargo).await.map(|_| ())
    }

    async fn run_uboot_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.run_uboot_artifact(&request, cargo, uboot).await
    }

    async fn quick_start_qemu(
        &mut self,
        platform: quick_start::QuickQemuPlatform,
        action: quick_start::QuickQemuAction,
    ) -> anyhow::Result<()> {
        let arch = platform.arch();

        match action {
            quick_start::QuickQemuAction::Build => {
                quick_start::refresh_qemu_build_config(self.app.workspace_root(), platform)?;
                rootfs::ensure_quick_start_qemu_rootfs(self.app.workspace_root(), arch).await?;
                let request = self.prepare_request(
                    Self::quick_start_build_args(
                        arch,
                        quick_start::tmp_qemu_build_config_path(
                            self.app.workspace_root(),
                            platform,
                        ),
                    ),
                    None,
                    None,
                    SnapshotPersistence::Store,
                )?;
                self.run_build_request(request).await
            }
            quick_start::QuickQemuAction::Run => {
                quick_start::ensure_qemu_build_config(self.app.workspace_root(), platform)?;
                let request = self.prepare_request(
                    Self::quick_start_build_args(
                        arch,
                        quick_start::tmp_qemu_build_config_path(
                            self.app.workspace_root(),
                            platform,
                        ),
                    ),
                    None,
                    None,
                    SnapshotPersistence::Store,
                )?;
                self.run_qemu_request(request).await
            }
        }
    }

    async fn quick_start_orangepi_build(
        &mut self,
        args: quick_start::QuickOrangeConfigArgs,
    ) -> anyhow::Result<()> {
        quick_start::prepare_orangepi_uboot_config(self.app.workspace_root(), &args)?;
        quick_start::ensure_orangepi_configs(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "aarch64",
                quick_start::tmp_orangepi_build_config_path(self.app.workspace_root()),
            ),
            None,
            None,
            SnapshotPersistence::Store,
        )?;
        self.run_build_request(request).await
    }

    async fn quick_start_orangepi_run(
        &mut self,
        args: quick_start::QuickOrangeRunArgs,
    ) -> anyhow::Result<()> {
        let uboot_config =
            quick_start::prepare_orangepi_uboot_config(self.app.workspace_root(), &args)?;
        quick_start::ensure_orangepi_configs(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "aarch64",
                quick_start::tmp_orangepi_build_config_path(self.app.workspace_root()),
            ),
            None,
            Some(uboot_config),
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }

    async fn quick_start_sg2002_build(&mut self) -> anyhow::Result<()> {
        quick_start::refresh_sg2002_config(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "riscv64",
                quick_start::tmp_sg2002_build_config_path(self.app.workspace_root()),
            ),
            None,
            None,
            SnapshotPersistence::Store,
        )?;
        self.run_build_request(request).await
    }

    async fn quick_start_sg2002_run(
        &mut self,
        args: quick_start::QuickSg2002RunArgs,
    ) -> anyhow::Result<()> {
        let uboot_config_path =
            quick_start::prepare_sg2002_uboot_config(self.app.workspace_root(), &args)?;
        quick_start::ensure_sg2002_config(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "riscv64",
                quick_start::tmp_sg2002_build_config_path(self.app.workspace_root()),
            ),
            None,
            Some(uboot_config_path),
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }
}

pub(crate) fn default_qemu_config_template_path(workspace_root: &Path, arch: &str) -> PathBuf {
    workspace_root.join(format!("os/StarryOS/configs/qemu/qemu-{arch}.toml"))
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
