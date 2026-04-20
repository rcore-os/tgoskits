use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use anyhow::Context;
use clap::{Args, Subcommand};
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
};

use crate::{
    command_flow::{self, SnapshotPersistence},
    context::{
        AppContext, DEFAULT_STARRY_ARCH, ResolvedStarryRequest, StarryCliArgs,
        starry_target_for_arch_checked,
    },
    test_qemu,
};

pub mod board;
pub mod build;
pub mod config;
pub mod quick_start;
pub mod rootfs;
pub mod test_suit;

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
    /// Run StarryOS test suites
    Test(ArgsTest),
    /// Download rootfs image into workspace target directory
    Rootfs(ArgsRootfs),
    /// Convenience entrypoints for common QEMU and Orange Pi workflows
    #[command(name = "quick-start")]
    QuickStart(quick_start::ArgsQuickStart),
    /// Build and run StarryOS application with U-Boot
    Uboot(ArgsUboot),
    /// Build and run StarryOS on a remote board
    Board(ArgsBoard),
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
pub struct ArgsRootfs {
    #[arg(long)]
    pub arch: Option<String>,
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

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run StarryOS QEMU test suite
    Qemu(ArgsTestQemu),
    /// Reserved StarryOS U-Boot test suite entrypoint
    Uboot(ArgsTestUboot),
    /// Run StarryOS remote board test suite
    Board(ArgsTestBoard),
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available board names
    Ls,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(short = 't', long, alias = "arch", value_name = "ARCH")]
    pub target: String,
    #[arg(short = 'c', long, value_name = "CASE")]
    pub test_case: Option<String>,
    #[arg(long, help = "Run stress StarryOS qemu test cases")]
    pub stress: bool,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestUboot;

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
    #[arg(short = 't', long = "test-group", value_name = "GROUP")]
    pub test_group: Option<String>,

    #[arg(long = "board-test-config")]
    pub board_test_config: Option<PathBuf>,

    #[arg(short = 'b', long = "board-type", value_name = "BOARD_TYPE")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,
}

pub struct Starry {
    app: AppContext,
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
            Command::Rootfs(args) => self.rootfs(args).await,
            Command::QuickStart(args) => self.quick_start(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Board(args) => self.board(args).await,
            Command::Test(args) => self.test(args).await,
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
            // Explicit rootfs provided: skip auto-download, apply directly.
            let rootfs = crate::download::resolve_rootfs_arg(
                self.app.workspace_root(),
                &request.arch,
                rootfs,
            );
            // If the path resolves into the unified rootfs dir, ensure the
            // tarball has been extracted (keyword paths need this).
            crate::download::ensure_unified_rootfs_if_managed(
                self.app.workspace_root(),
                &request.arch,
                &rootfs,
            )
            .await?;
            self.app.set_debug_mode(request.debug)?;
            let cargo = build::load_cargo_config(&request)?;
            let mut qemu = self.load_qemu_config(&request, &cargo, false).await?;
            rootfs::apply_disk_image_qemu_args(&mut qemu, rootfs);
            rootfs::apply_smp_qemu_arg(&mut qemu, request.smp);
            self.app
                .qemu(cargo, request.build_info_path, Some(qemu))
                .await
        } else {
            self.run_qemu_request(request).await
        }
    }

    async fn rootfs(&mut self, args: ArgsRootfs) -> anyhow::Result<()> {
        let arch = args.arch.unwrap_or_else(|| DEFAULT_STARRY_ARCH.to_string());
        let target = starry_target_for_arch_checked(&arch)?.to_string();
        let disk_img =
            rootfs::ensure_rootfs_in_target_dir(self.app.workspace_root(), &arch, &target).await?;
        println!("rootfs ready at {}", disk_img.display());
        Ok(())
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
        use quick_start::{QuickOrangeAction, QuickQemuPlatform, QuickStartCommand};

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
        }
    }
    async fn test(&mut self, args: ArgsTest) -> anyhow::Result<()> {
        match args.command {
            TestCommand::Qemu(args) => self.test_qemu(args).await,
            TestCommand::Uboot(args) => self.test_uboot(args).await,
            TestCommand::Board(args) => self.test_board(args).await,
        }
    }

    async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        let (arch, target) = test_suit::parse_test_target(self.app.workspace_root(), &args.target)?;
        let test_group = if args.stress {
            test_suit::StarryTestGroup::Stress
        } else {
            test_suit::StarryTestGroup::Normal
        };
        let cases = test_suit::discover_qemu_cases(
            self.app.workspace_root(),
            &arch,
            args.test_case.as_deref(),
            test_group,
        )?;
        let package = crate::context::STARRY_PACKAGE;

        println!(
            "running starry {} qemu tests for package {} on arch: {} (target: {})",
            test_group.as_str(),
            package,
            arch,
            target
        );

        let default_board = board::default_board_for_target(self.app.workspace_root(), &target)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "missing Starry qemu defconfig for target `{target}` in tests; expected a \
                     default qemu board config under os/StarryOS/configs/board"
                )
            })?;
        let mut request = self.prepare_request(
            Self::test_build_args(&arch),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        request.plat_dyn = Some(default_board.build_info.plat_dyn);
        request.build_info_override = Some(default_board.build_info.clone());
        rootfs::ensure_rootfs_in_target_dir(
            self.app.workspace_root(),
            &request.arch,
            &request.target,
        )
        .await?;
        let cargo = build::load_cargo_config(&request)?;

        let total = cases.len();
        let suite_started = Instant::now();
        let mut reports = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            println!("[{}/{}] starry qemu {}", index + 1, total, case.name);

            let case_started = Instant::now();
            match self
                .run_qemu_case(&request, &cargo, case)
                .await
                .with_context(|| format!("starry qemu test failed for case `{}`", case.name))
            {
                Ok(()) => {
                    println!("ok: {}", case.name);
                    reports.push(test_suit::StarryQemuCaseReport {
                        name: case.name.clone(),
                        outcome: test_suit::StarryQemuCaseOutcome::Passed,
                        duration: case_started.elapsed(),
                    });
                }
                Err(err) => {
                    eprintln!("failed: {}: {:#}", case.name, err);
                    reports.push(test_suit::StarryQemuCaseReport {
                        name: case.name.clone(),
                        outcome: test_suit::StarryQemuCaseOutcome::Failed,
                        duration: case_started.elapsed(),
                    });
                }
            }
        }

        test_suit::finalize_qemu_case_run(&test_suit::StarryQemuRunReport {
            group: test_group,
            cases: reports,
            total_duration: suite_started.elapsed(),
        })
    }

    async fn test_uboot(&mut self, _args: ArgsTestUboot) -> anyhow::Result<()> {
        test_qemu::unsupported_uboot_test_command("starry")
    }

    async fn test_board(&mut self, args: ArgsTestBoard) -> anyhow::Result<()> {
        ensure_board_test_args(&args)?;

        if let Some(path) = args.board_test_config.as_ref()
            && !path.exists()
        {
            anyhow::bail!("missing explicit board test config `{}`", path.display());
        }

        let groups = test_suit::discover_board_test_groups(
            self.app.workspace_root(),
            args.test_group.as_deref(),
        )?;
        let total = groups.len();
        let mut failed = Vec::new();

        for (index, group) in groups.into_iter().enumerate() {
            let board_test_config = args
                .board_test_config
                .clone()
                .unwrap_or_else(|| group.board_test_config_path.clone());
            let board_test_config_summary = board_test_config.display().to_string();

            if !board_test_config.exists() {
                eprintln!(
                    "failed: {}: missing board test config `{}`",
                    group.name, board_test_config_summary
                );
                failed.push(group.name.clone());
                continue;
            }

            println!("[{}/{}] starry board {}", index + 1, total, group.name);

            let result = async {
                let request = self.prepare_request(
                    Self::test_board_build_args(&group),
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
                            "starry board test failed for group `{}` (build_config={}, \
                             board_test_config={})",
                            group.name,
                            group.build_config_path.display(),
                            board_test_config_summary
                        )
                    })
            }
            .await;

            match result {
                Ok(()) => println!("ok: {}", group.name),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", group.name, err);
                    failed.push(group.name);
                }
            }
        }

        test_suit::finalize_board_test_run(&failed)
    }

    fn prepare_request(
        &self,
        args: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedStarryRequest> {
        let (request, snapshot) =
            self.app
                .prepare_starry_request(args, qemu_config, uboot_config)?;
        if matches!(persistence, SnapshotPersistence::Store) {
            self.app.store_starry_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    fn test_build_args(arch: &str) -> StarryCliArgs {
        StarryCliArgs {
            config: None,
            arch: Some(arch.to_string()),
            target: None,
            smp: None,
            debug: false,
        }
    }

    fn test_board_build_args(group: &test_suit::StarryBoardTestGroup) -> StarryCliArgs {
        StarryCliArgs {
            config: Some(group.build_config_path.clone()),
            arch: None,
            target: Some(group.target.clone()),
            smp: None,
            debug: false,
        }
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

    async fn load_qemu_config(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
        apply_default_args: bool,
    ) -> anyhow::Result<ostool::run::qemu::QemuConfig> {
        let mut qemu = match request.qemu_config.as_deref() {
            Some(path) => {
                self.app
                    .tool_mut()
                    .read_qemu_config_from_path_for_cargo(cargo, path)
                    .await?
            }
            None => {
                self.app
                    .tool_mut()
                    .ensure_qemu_config_for_cargo(cargo)
                    .await?
            }
        };

        if request.qemu_config.is_none() && apply_default_args {
            rootfs::apply_default_qemu_args(self.app.workspace_root(), request, &mut qemu).await?;
        }
        rootfs::apply_smp_qemu_arg(&mut qemu, request.smp);

        Ok(qemu)
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
    ) -> anyhow::Result<Option<ostool::run::uboot::UbootConfig>> {
        match request.uboot_config.as_deref() {
            Some(path) => self
                .app
                .tool_mut()
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
                    .tool_mut()
                    .read_board_run_config_from_path_for_cargo(cargo, path)
                    .await
            }
            None => {
                let workspace_root = self.app.workspace_root().to_path_buf();
                self.app
                    .tool_mut()
                    .ensure_board_run_config_in_dir_for_cargo(cargo, &workspace_root)
                    .await
            }
        }
    }

    async fn run_qemu_case(
        &mut self,
        request: &ResolvedStarryRequest,
        cargo: &Cargo,
        case: &test_suit::StarryQemuCase,
    ) -> anyhow::Result<()> {
        let mut case_request = request.clone();
        let mut qemu = self
            .app
            .tool_mut()
            .read_qemu_config_from_path_for_cargo(cargo, &case.qemu_config_path)
            .await?;

        if case_request.smp.is_none() {
            case_request.smp = rootfs::smp_from_qemu_arg(&qemu);
        }
        let cargo = if case_request.smp != request.smp {
            build::load_cargo_config(&case_request)?
        } else {
            cargo.clone()
        };

        let case_assets = rootfs::prepare_case_assets(
            self.app.workspace_root(),
            &case_request.arch,
            &case_request.target,
            case,
        )
        .await?;
        rootfs::apply_disk_image_qemu_args(&mut qemu, case_assets.rootfs_path);
        qemu.args.extend(case_assets.extra_qemu_args);
        rootfs::apply_smp_qemu_arg(&mut qemu, case_request.smp);

        self.app
            .qemu(cargo, case_request.build_info_path, Some(qemu))
            .await
    }

    async fn run_qemu_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        let qemu = self.load_qemu_config(&request, &cargo, true).await?;
        self.app
            .qemu(cargo, request.build_info_path, Some(qemu))
            .await
    }

    async fn run_build_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
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
                let target = starry_target_for_arch_checked(arch)?.to_string();
                quick_start::refresh_qemu_configs(self.app.workspace_root(), platform)?;
                rootfs::ensure_rootfs_in_target_dir(self.app.workspace_root(), arch, &target)
                    .await?;
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
                quick_start::ensure_qemu_configs(self.app.workspace_root(), platform)?;
                let request = self.prepare_request(
                    Self::quick_start_build_args(
                        arch,
                        quick_start::tmp_qemu_build_config_path(
                            self.app.workspace_root(),
                            platform,
                        ),
                    ),
                    Some(quick_start::tmp_qemu_run_config_path(
                        self.app.workspace_root(),
                        platform,
                    )),
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
        quick_start::refresh_orangepi_configs(self.app.workspace_root())?;
        quick_start::prepare_orangepi_uboot_config(self.app.workspace_root(), &args)?;
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
        quick_start::ensure_orangepi_configs(self.app.workspace_root())?;
        let request = self.prepare_request(
            Self::quick_start_build_args(
                "aarch64",
                quick_start::tmp_orangepi_build_config_path(self.app.workspace_root()),
            ),
            None,
            Some(quick_start::prepare_orangepi_uboot_config(
                self.app.workspace_root(),
                &args,
            )?),
            SnapshotPersistence::Store,
        )?;
        self.run_uboot_request(request).await
    }
}

impl Default for Starry {
    fn default() -> Self {
        Self::new().expect("failed to initialize StarryOS")
    }
}

fn ensure_board_test_args(args: &ArgsTestBoard) -> anyhow::Result<()> {
    if args.board_test_config.is_some() && args.test_group.is_none() {
        anyhow::bail!(
            "`--board-test-config` requires `--test-group` because board test configs embed a \
             single board_type"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

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
                    assert_eq!(args.target, "x86_64");
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
    fn command_parses_test_uboot() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["starry", "test", "uboot"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Uboot(_) => {}
                _ => panic!("expected uboot test command"),
            },
            _ => panic!("expected test command"),
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
            "-t",
            "smoke-orangepi-5-plus",
            "-b",
            "OrangePi-5-Plus",
            "--board-test-config",
            "board-test.toml",
            "--server",
            "10.0.0.2",
            "--port",
            "9000",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Board(args) => {
                    assert_eq!(args.test_group.as_deref(), Some("smoke-orangepi-5-plus"));
                    assert_eq!(args.board_type.as_deref(), Some("OrangePi-5-Plus"));
                    assert_eq!(
                        args.board_test_config,
                        Some(PathBuf::from("board-test.toml"))
                    );
                    assert_eq!(args.server.as_deref(), Some("10.0.0.2"));
                    assert_eq!(args.port, Some(9000));
                }
                _ => panic!("expected board test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn board_test_requires_group_when_override_config_is_present() {
        let err = ensure_board_test_args(&ArgsTestBoard {
            test_group: None,
            board_test_config: Some(PathBuf::from("board-test.toml")),
            board_type: None,
            server: None,
            port: None,
        })
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("`--board-test-config` requires `--test-group`")
        );
    }

    #[test]
    fn command_parses_test_qemu_with_case_and_stress() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "starry", "test", "qemu", "-t", "x86_64", "-c", "smoke", "--stress",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.target, "x86_64");
                    assert_eq!(args.test_case, Some("smoke".to_string()));
                    assert!(args.stress);
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
