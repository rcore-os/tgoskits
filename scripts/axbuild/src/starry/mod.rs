use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand};
use ostool::build::CargoQemuOverrideArgs;

use crate::{
    command_flow::{self, SnapshotPersistence},
    context::{
        AppContext, DEFAULT_STARRY_ARCH, QemuRunConfig, ResolvedStarryRequest, StarryCliArgs,
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
}

#[derive(Args, Clone)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(long)]
    pub arch: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,

    #[arg(long)]
    pub debug: bool,
}

#[derive(Args)]
pub struct ArgsQemu {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub qemu_config: Option<PathBuf>,
}

#[derive(Args)]
pub struct ArgsUboot {
    #[command(flatten)]
    pub build: ArgsBuild,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
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

pub struct Starry {
    app: AppContext,
}

impl From<&ArgsBuild> for StarryCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
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
        self.run_qemu_request(request).await
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
        let mut failed = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            println!("[{}/{}] starry qemu {}", index + 1, total, case.name);

            match self
                .app
                .qemu(
                    cargo.clone(),
                    request.build_info_path.clone(),
                    Self::test_qemu_run_config(case.qemu_config_path.clone()),
                )
                .await
                .with_context(|| format!("starry qemu test failed for case `{}`", case.name))
            {
                Ok(()) => println!("ok: {}", case.name),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", case.name, err);
                    failed.push(case.name.clone());
                }
            }
        }

        test_suit::finalize_qemu_case_run(&failed, test_group)
    }

    async fn test_uboot(&mut self, _args: ArgsTestUboot) -> anyhow::Result<()> {
        test_qemu::unsupported_uboot_test_command("starry")
    }

    fn prepare_request(
        &self,
        args: StarryCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedStarryRequest> {
        command_flow::resolve_request(
            persistence,
            || {
                self.app
                    .prepare_starry_request(args, qemu_config, uboot_config)
            },
            |snapshot| self.app.store_starry_snapshot(snapshot),
        )
    }

    fn test_build_args(arch: &str) -> StarryCliArgs {
        StarryCliArgs {
            config: None,
            arch: Some(arch.to_string()),
            target: None,
            debug: false,
        }
    }

    fn quick_start_build_args(arch: &str, config: PathBuf) -> StarryCliArgs {
        StarryCliArgs {
            config: Some(config),
            arch: Some(arch.to_string()),
            target: None,
            debug: false,
        }
    }

    fn qemu_run_config(
        qemu_config: Option<PathBuf>,
        qemu_args: Vec<String>,
    ) -> anyhow::Result<QemuRunConfig> {
        Ok(QemuRunConfig {
            qemu_config,
            default_args: CargoQemuOverrideArgs {
                args: Some(qemu_args),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn test_qemu_run_config(qemu_config: PathBuf) -> QemuRunConfig {
        QemuRunConfig {
            qemu_config: Some(qemu_config),
            ..Default::default()
        }
    }

    async fn run_qemu_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        let qemu_args = rootfs::default_qemu_args(self.app.workspace_root(), &request).await?;
        self.run_qemu_request_with_args(request, qemu_args).await
    }

    async fn run_qemu_request_with_args(
        &mut self,
        request: ResolvedStarryRequest,
        qemu_args: Vec<String>,
    ) -> anyhow::Result<()> {
        command_flow::run_qemu(
            &mut self.app,
            request,
            build::load_cargo_config,
            move |request| Self::qemu_run_config(request.qemu_config.clone(), qemu_args),
        )
        .await
    }

    async fn run_build_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedStarryRequest) -> anyhow::Result<()> {
        command_flow::run_uboot(&mut self.app, request, build::load_cargo_config).await
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
    fn test_qemu_run_config_uses_case_path_verbatim() {
        let path = PathBuf::from("test-suit/starryos/normal/smoke/qemu-riscv64.toml");
        let config = Starry::test_qemu_run_config(path.clone());

        assert_eq!(config.qemu_config, Some(path));
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
}
