use std::path::{Path, PathBuf};

use clap::{Args, Subcommand, ValueEnum};
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
};

use crate::{
    context::{AppContext, ResolvedStarryRequest, SnapshotPersistence, StarryCliArgs},
    test::{case as qemu_case, qemu},
};

pub(crate) mod apk;
pub mod app;
pub mod board;
pub mod build;
pub mod config;
pub mod kmod;
pub mod perf;
pub mod quick_start;
pub(crate) mod resolver;
pub mod rootfs;
pub mod test;

/// StarryOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build StarryOS application
    Build(ArgsBuild),
    /// Build and run StarryOS application
    Qemu(ArgsQemu),
    /// Generate a default StarryOS board config
    Defconfig(ArgsDefconfig),
    /// StarryOS board config helpers
    Config(ArgsConfig),
    /// Build and profile StarryOS with qperf
    Perf(ArgsPerf),
    /// Run StarryOS test suites
    Test(test::ArgsTest),
    /// Run StarryOS runnable apps
    App(app::ArgsApp),
    /// Download rootfs image into workspace target directory
    Rootfs(rootfs::ArgsRootfs),
    /// Convenience entrypoints for common QEMU and Orange Pi workflows
    #[command(name = "quick-start")]
    QuickStart(quick_start::ArgsQuickStart),
    /// Build and run StarryOS application with U-Boot
    Uboot(ArgsUboot),
    /// Build and run StarryOS on a remote board
    Board(ArgsBoard),
    /// Build StarryOS loadable kernel modules (`.ko`)
    Kmod(kmod::ArgsKmod),
}

#[derive(Args, Clone)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,

    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,

    /// Override the rootfs disk image path (skips auto-download).
    #[arg(long, value_name = "IMAGE")]
    pub rootfs: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsPerf {
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(long, default_value_t = 99)]
    pub freq: u32,
    #[arg(long)]
    pub out: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = PerfFormat::All)]
    pub format: PerfFormat,
    #[arg(long, default_value_t = 64)]
    pub max_depth: usize,
    #[arg(long, value_name = "SECONDS", default_value_t = 20)]
    pub timeout: u64,
    #[arg(long, default_value = "tb")]
    pub mode: String,
    #[arg(long, default_value_t = 20)]
    pub top: usize,
    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PerfFormat {
    Folded,
    Svg,
    Pprof,
    All,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsBoard {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long = "board-config")]
    pub board_config: Option<PathBuf>,

    #[arg(short = 'b', long)]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,
}

#[derive(Args)]
pub struct ArgsDefconfig {
    pub board: String,
}

#[derive(Args)]
pub struct ArgsConfig {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available board names
    Ls,
}

pub struct Starry {
    pub(super) app: AppContext,
}

impl From<&ArgsBuild> for StarryCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            smp: args.smp,
            debug: args.debug,
        }
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
        let request = self.prepare_request(
            (&args.build).into(),
            args.qemu_config,
            None,
            SnapshotPersistence::Store,
        )?;
        if let Some(board) = config::ensure_default_build_config_for_target(
            self.app.workspace_root(),
            &request.target,
            &request.build_info_path,
        )? {
            println!(
                "generated missing Starry qemu build config {} from board {}",
                request.build_info_path.display(),
                board.name
            );
        }
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
        self.app
            .board(
                cargo,
                request.build_info_path,
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
            let rootfs_path = crate::rootfs::store::resolve_explicit_rootfs(
                self.app.workspace_root(),
                &request.arch,
                case.rootfs_path,
            );
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
            println!("  prepare assets: 0ns (pipeline=plain, cache=miss)");
            println!(
                "  qemu config: {} (timeout={})",
                test_case.qemu_config_path.display(),
                qemu::qemu_timeout_summary(&qemu)
            );
            println!("  rootfs: {}", rootfs_path.display());
            return self
                .app
                .qemu(cargo, request.build_info_path, Some(qemu))
                .await;
        }
        let rootfs_path = crate::rootfs::store::resolve_explicit_rootfs(
            self.app.workspace_root(),
            &request.arch,
            case.rootfs_path,
        );
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

        let result = self
            .app
            .qemu(cargo, request.build_info_path, Some(qemu))
            .await;
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
        board_config.shell_init_cmd = Some(case.init_cmd);
        self.app
            .board(
                cargo,
                request.build_info_path,
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

    async fn run_qemu_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        rootfs::qemu(self, request).await
    }

    async fn run_build_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let uboot = self.load_uboot_config(&request, &cargo).await?;
        self.app.uboot(cargo, request.build_info_path, uboot).await
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::starry::test::TestCommand;

    #[test]
    fn command_parses_test_qemu() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "test", "qemu", "--target", "x86_64"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.target.as_deref(), Some("x86_64"));
                    assert_eq!(args.test_group, None);
                    assert!(!args.stress);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_defconfig() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "defconfig", "qemu-aarch64"]).unwrap();

        match cli.command {
            Command::Defconfig(args) => assert_eq!(args.board, "qemu-aarch64"),
            _ => panic!("expected defconfig command"),
        }
    }

    #[test]
    fn command_parses_config_ls() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "config", "ls"]).unwrap();

        match cli.command {
            Command::Config(args) => match args.command {
                ConfigCommand::Ls => {}
            },
            _ => panic!("expected config ls command"),
        }
    }

    #[test]
    fn command_parses_test_board() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "test",
            "board",
            "-g",
            "normal",
            "-c",
            "smoke",
            "--board",
            "orangepi-5-plus",
            "-b",
            "OrangePi-5-Plus",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Board(args) => {
                    assert_eq!(args.test_group.as_deref(), Some("normal"));
                    assert_eq!(args.test_case.as_deref(), Some("smoke"));
                    assert_eq!(args.board.as_deref(), Some("orangepi-5-plus"));
                    assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                    assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                    assert_eq!(args.port, Some(9000));
                }
                _ => panic!("expected board test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_with_group_and_case() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry", "test", "qemu", "--arch", "x86_64", "-g", "stress", "-c", "smoke",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch.as_deref(), Some("x86_64"));
                    assert_eq!(args.target, None);
                    assert_eq!(args.test_group.as_deref(), Some("stress"));
                    assert_eq!(args.test_case, Some("smoke".to_string()));
                    assert!(!args.stress);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_with_stress_alias() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "test", "qemu", "--arch", "x86_64", "--stress"])
            .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch.as_deref(), Some("x86_64"));
                    assert_eq!(args.test_group, None);
                    assert!(args.stress);
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_with_target() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli =
            Cli::try_parse_from(["starry", "test", "qemu", "--target", "x86_64-unknown-none"])
                .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch, None);
                    assert_eq!(args.target.as_deref(), Some("x86_64-unknown-none"));
                }
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_rejects_removed_shell_init_cmd_flag() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(
            Cli::try_parse_from([
                "starry",
                "test",
                "qemu",
                "--target",
                "x86_64",
                "--shell-init-cmd",
                "echo test",
            ])
            .is_err()
        );
    }

    #[test]
    fn command_rejects_removed_timeout_flag() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(
            Cli::try_parse_from([
                "starry",
                "test",
                "qemu",
                "--target",
                "x86_64",
                "--timeout",
                "10",
            ])
            .is_err()
        );
    }

    #[test]
    fn command_parses_quick_start_qemu_build() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "quick-start", "qemu-aarch64", "build"]).unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::QemuAarch64(inner) => {
                    assert!(matches!(inner.action, quick_start::QuickQemuAction::Build));
                }
                _ => panic!("expected qemu-aarch64 quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn command_rejects_removed_plat_dyn_flag() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(Cli::try_parse_from(["starry", "qemu", "--plat-dyn"]).is_err());
    }

    #[test]
    fn command_parses_quick_start_orangepi_run() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "quick-start",
            "orangepi-5-plus",
            "run",
            "--serial",
            "/dev/ttyUSB0",
            "--baud",
            "1500000",
        ])
        .unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::Orangepi5Plus(inner) => match inner.action {
                    quick_start::QuickOrangeAction::Run(run) => {
                        assert_eq!(run.serial.as_deref(), Some("/dev/ttyUSB0"));
                        assert_eq!(run.baud.as_deref(), Some("1500000"));
                    }
                    _ => panic!("expected orangepi run quick-start command"),
                },
                _ => panic!("expected orangepi quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn command_parses_quick_start_orangepi_build_with_overrides() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "quick-start",
            "orangepi-5-plus",
            "build",
            "--serial",
            "/dev/ttyUSB0",
        ])
        .unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::Orangepi5Plus(inner) => match inner.action {
                    quick_start::QuickOrangeAction::Build(build) => {
                        assert_eq!(build.serial.as_deref(), Some("/dev/ttyUSB0"));
                    }
                    _ => panic!("expected orangepi build quick-start command"),
                },
                _ => panic!("expected orangepi quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn command_parses_quick_start_sg2002_local_run() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "quick-start",
            "licheerv-nano-sg2002",
            "run",
            "--serial",
            "/dev/ttyUSB1",
            "--baud",
            "115200",
        ])
        .unwrap();

        match cli.command {
            Command::QuickStart(args) => match args.command {
                quick_start::QuickStartCommand::LicheervNanoSg2002(inner) => match inner.action {
                    quick_start::QuickSg2002Action::Run(run) => {
                        assert_eq!(run.serial.as_deref(), Some("/dev/ttyUSB1"));
                        assert_eq!(run.baud.as_deref(), Some("115200"));
                    }
                    _ => panic!("expected sg2002 run quick-start command"),
                },
                _ => panic!("expected sg2002 quick-start command"),
            },
            _ => panic!("expected quick-start command"),
        }
    }

    #[test]
    fn formats_starry_app_run_progress() {
        assert_eq!(
            format_app_run_progress(3, 12, "qemu/sqlite", Some("x86_64")),
            "RUN\t3/12\tqemu/sqlite\tarch=x86_64"
        );
        assert_eq!(
            format_app_run_progress(1, 1, "deepseek-tui", None),
            "RUN\t1/1\tdeepseek-tui"
        );
    }

    #[test]
    fn command_parses_app_board() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "app",
            "board",
            "-t",
            "orangepi-5-plus-uvc",
            "-b",
            "OrangePi-5-Plus",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
            "--debug",
        ])
        .unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::Board(args) => {
                    assert_eq!(args.test_case, "orangepi-5-plus-uvc");
                    assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                    assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                    assert_eq!(args.port, Some(9000));
                    assert!(args.debug);
                }
                _ => panic!("expected app board command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_parses_app_board_with_long_case_and_config() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "app",
            "board",
            "--test-case",
            "orangepi-5-plus-uvc",
            "--board-config",
            "board.toml",
        ])
        .unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::Board(args) => {
                    assert_eq!(args.test_case, "orangepi-5-plus-uvc");
                    assert_eq!(args.board_config, Some(PathBuf::from("board.toml")));
                }
                _ => panic!("expected app board command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_parses_app_list() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "app", "list", "--kind", "qemu"]).unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::List(args) => {
                    assert_eq!(args.kind, Some(app::StarryAppKind::Qemu))
                }
                _ => panic!("expected app list command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_parses_app_qemu_all() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "app",
            "qemu",
            "--all",
            "--cap",
            "board:OrangePi-5-Plus",
            "--arch",
            "x86_64",
            "--qemu-config",
            "qemu.toml",
            "--debug",
        ])
        .unwrap();

        match cli.command {
            Command::App(args) => match args.command {
                app::AppCommand::Qemu(args) => {
                    assert!(args.all);
                    assert_eq!(args.caps, vec!["board:OrangePi-5-Plus"]);
                    assert_eq!(args.arch.as_deref(), Some("x86_64"));
                    assert_eq!(args.qemu_config, Some(PathBuf::from("qemu.toml")));
                    assert!(args.debug);
                }
                _ => panic!("expected app qemu command"),
            },
            _ => panic!("expected app command"),
        }
    }

    #[test]
    fn command_rejects_app_board_without_case() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        assert!(Cli::try_parse_from(["starry", "app", "board"]).is_err());
    }

    #[test]
    fn command_parses_board() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry",
            "board",
            "--arch",
            "aarch64",
            "--board-config",
            "remote.board.toml",
            "-b",
            "rk3568",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            Command::Board(args) => {
                assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.board_config, Some(PathBuf::from("remote.board.toml")));
                assert_eq!(args.board_type.as_deref(), Some("rk3568"));
                assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                assert_eq!(args.port, Some(9000));
            }
            _ => panic!("expected board command"),
        }
    }
}
