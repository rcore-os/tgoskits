use std::path::{Path, PathBuf};

use ostool::{
    board::{BoardRunRequest, RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
};

use crate::context::{
    AppContext, RequestEnvironment, ResolvedStarryRequest, SnapshotPersistence, StarryCliArgs,
    board_run_request,
};

pub(crate) mod apk;
pub use crate::app::starry as app;
mod args;
pub(crate) mod boot_entropy;
pub mod build;
pub mod config;
pub mod kmod;
pub(crate) mod openrc;
pub use crate::profiling::starry as perf;
pub mod quick_start;
pub(crate) mod resolver;
pub mod rootfs;
pub use args::*;

pub use crate::test::normal::starry as test;

pub struct Starry {
    pub(super) app: AppContext,
}

#[cfg(test)]
pub(crate) fn test_request(path: &Path, arch: &str, target: &str) -> ResolvedStarryRequest {
    use crate::{
        build::{ConfigInput, ConfigSource},
        context::STARRY_PACKAGE,
    };

    let input: ConfigInput<config::StarryConfig> = if path.is_file() {
        crate::build::load_config_input(Path::new("/"), Some(path), None).unwrap()
    } else {
        ConfigInput {
            config: config::StarryConfig::default(),
            source: ConfigSource::File {
                path: path.to_path_buf(),
                preset: None,
            },
            target_constraint: None,
            selection_preset: None,
            file_fields: Default::default(),
        }
    };
    let mut config = input.config;
    config.common.target = Some(target.to_string());
    let runtime_env = ax_config::RuntimeEnv::default();
    let runtime = ax_config::resolve_runtime(
        config.common.max_cpu_num,
        config.common.runtime,
        &runtime_env,
    )
    .unwrap();
    ResolvedStarryRequest {
        package: STARRY_PACKAGE.to_string(),
        arch: arch.to_string(),
        target: target.to_string(),
        smp: None,
        debug: false,
        source: input.source,
        config,
        runtime,
        runtime_env,
        environment_features: Vec::new(),
        qemu_config: None,
        uboot_config: None,
    }
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
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let write_policy = args.resolved_rootfs_write_policy();
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        if let Some(rootfs) = args.rootfs {
            rootfs::qemu_with_explicit_rootfs(self, request, rootfs, write_policy).await
        } else {
            self.run_qemu_request(request, write_policy).await
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
                for preset in config::presets::PRESETS {
                    println!(
                        "{}\t{}\t{}",
                        preset.name, preset.common.board.id, preset.common.board.target
                    );
                }
            }
            ConfigCommand::Show(args) => {
                let request =
                    self.prepare_request((&args).into(), None, None, SnapshotPersistence::Discard)?;
                crate::build::show_config(
                    &request.config,
                    &request.source,
                    request.runtime,
                    &request.environment_features,
                )?;
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
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        let (board_config, board_config_path) = self
            .load_board_config(&cargo, args.board_config.as_deref())
            .await?;
        self.run_board_artifact(
            &request,
            cargo,
            board_config,
            board_config_path,
            None,
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

    pub(super) fn prepare_request(
        &self,
        args: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedStarryRequest> {
        let (request, snapshot) =
            self.app
                .prepare_starry_request(args, qemu_config, uboot_config)?;
        if persistence.should_store() {
            self.app.store_starry_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    pub(super) fn prepare_request_from_input(
        &self,
        args: StarryCliArgs,
        input: crate::build::ConfigInput<crate::starry::config::StarryConfig>,
        environment: RequestEnvironment,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedStarryRequest> {
        let (request, snapshot) = self.app.prepare_starry_request_from_input(
            args,
            input,
            environment,
            qemu_config,
            uboot_config,
        )?;
        if persistence.should_store() {
            self.app.store_starry_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    fn quick_start_build_args(preset: &str) -> StarryCliArgs {
        StarryCliArgs {
            preset: Some(preset.to_string()),
            config: None,
            arch: None,
            target: None,
            smp: None,
            debug: false,
        }
    }

    pub(crate) async fn load_uboot_config(
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

    pub(crate) async fn load_board_config(
        &mut self,
        cargo: &Cargo,
        board_config_path: Option<&Path>,
    ) -> anyhow::Result<(BoardRunConfig, PathBuf)> {
        match board_config_path {
            Some(path) => self
                .app
                .read_board_run_config_from_path_for_cargo(cargo, path)
                .await
                .map(|config| (config, path.to_path_buf())),
            None => {
                let workspace_root = self.app.workspace_root().to_path_buf();
                self.app
                    .ensure_board_run_config_in_dir_for_cargo(cargo, &workspace_root)
                    .await
                    .map(|config| (config, workspace_root.join(".board.toml")))
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
        mut qemu: ostool::run::qemu::QemuConfig,
    ) -> anyhow::Result<()> {
        let output = self.build_artifact(request, cargo.clone()).await?;
        crate::execution::qemu::append_backtrace_map(
            &mut qemu,
            &output.elf_path().with_extension("axbt"),
            &request.target,
        )?;
        self.app.run_qemu(&cargo, qemu).await
    }

    pub(super) async fn run_uboot_artifact(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: Cargo,
        uboot: Option<ostool::run::uboot::UbootConfig>,
    ) -> anyhow::Result<()> {
        let mut uboot = match uboot {
            Some(uboot) => uboot,
            None => self.app.ensure_uboot_config_for_cargo(&cargo).await?,
        };
        let output = self.build_artifact(request, cargo).await?;
        crate::execution::qemu::append_backtrace_map_to_initramfs(
            &mut uboot.boot.initramfs,
            &output.elf_path().with_extension("axbt"),
        )?;
        self.app.run_prepared_uboot(uboot).await
    }

    pub(crate) async fn run_board_artifact(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: Cargo,
        mut board_config: BoardRunConfig,
        board_config_path: PathBuf,
        session_assets: Option<test::PreparedBoardSessionAssets>,
        options: RunBoardOptions,
    ) -> anyhow::Result<()> {
        let _boot_entropy = boot_entropy::prepare_for_secure_wifi(&mut board_config)?;
        let output = self.build_artifact(request, cargo.clone()).await?;
        crate::execution::qemu::append_backtrace_map_to_initramfs(
            &mut board_config.boot.initramfs,
            &output.elf_path().with_extension("axbt"),
        )?;
        let board_request = match session_assets {
            Some(assets) => {
                println!(
                    "[axbuild] board session upload root: {}",
                    assets.root.display()
                );
                BoardRunRequest::new(board_config, options)
                    .with_session_files(&assets.root, &assets.relative_paths)?
            }
            None => board_run_request(&board_config_path, board_config, options)?,
        };
        self.app
            .board_prepared_elf_with_request(
                output.elf_path().to_path_buf(),
                request.config_path().map(Path::to_path_buf),
                board_request,
            )
            .await
    }

    async fn run_qemu_request(
        &mut self,
        request: ResolvedStarryRequest,
        write_policy: rootfs::RootfsWritePolicy,
    ) -> anyhow::Result<()> {
        rootfs::qemu(self, request, write_policy).await
    }

    async fn run_build_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        self.build_artifact(&request, cargo).await.map(|_| ())
    }

    async fn run_uboot_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
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
                rootfs::ensure_quick_start_qemu_rootfs(
                    self.app.workspace_root(),
                    self.app.target_dir(),
                    arch,
                )
                .await?;
                let request = self.prepare_request(
                    Self::quick_start_build_args(platform.cli_name()),
                    None,
                    None,
                    SnapshotPersistence::Store,
                )?;
                self.run_build_request(request).await
            }
            quick_start::QuickQemuAction::Run => {
                let request = self.prepare_request(
                    Self::quick_start_build_args(platform.cli_name()),
                    None,
                    None,
                    SnapshotPersistence::Store,
                )?;
                self.run_qemu_request(request, rootfs::RootfsWritePolicy::Discard)
                    .await
            }
        }
    }

    async fn quick_start_orangepi_build(
        &mut self,
        args: quick_start::QuickOrangeConfigArgs,
    ) -> anyhow::Result<()> {
        quick_start::prepare_orangepi_uboot_config(self.app.workspace_root(), &args)?;
        quick_start::ensure_orangepi_uboot_config(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args("orangepi-5-plus"),
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
        let request = self.prepare_request(
            Self::quick_start_build_args("orangepi-5-plus"),
            None,
            Some(uboot_config),
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }

    async fn quick_start_sg2002_build(&mut self) -> anyhow::Result<()> {
        quick_start::ensure_sg2002_uboot_config(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args("licheerv-nano-sg2002"),
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
        let request = self.prepare_request(
            Self::quick_start_build_args("licheerv-nano-sg2002"),
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
