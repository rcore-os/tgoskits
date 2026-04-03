use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand};

use crate::{
    axvisor::context::AxvisorContext,
    command_flow::{self, SnapshotPersistence},
    context::{AppContext, AxvisorCliArgs, QemuRunConfig, ResolvedAxvisorRequest},
    test_qemu,
};

pub mod board;
pub mod build;
pub mod config;
pub mod context;
pub mod image;
pub mod qemu;
pub mod qemu_test;

/// Axvisor host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Build Axvisor
    Build(ArgsBuild),
    /// Build and run Axvisor in QEMU
    Qemu(ArgsQemu),
    /// Build and run Axvisor with U-Boot
    Uboot(ArgsUboot),
    /// Generate a default board config
    Defconfig(ArgsDefconfig),
    /// Board config helpers
    Config(ArgsConfig),
    /// Guest image management
    Image(image::Args),
    /// Run Axvisor test suites
    Test(ArgsTest),
}

#[derive(Args, Clone)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,

    #[arg(long)]
    pub arch: Option<String>,

    #[arg(short, long)]
    pub target: Option<String>,

    #[arg(long = "plat_dyn", alias = "plat-dyn")]
    pub plat_dyn: Option<bool>,

    #[arg(long)]
    pub vmconfigs: Vec<PathBuf>,
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
    /// Run Axvisor QEMU test suite
    Qemu(ArgsTestQemu),
    /// Run Axvisor U-Boot board test suite
    Uboot(ArgsTestUboot),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(long, alias = "arch", value_name = "ARCH")]
    pub target: String,
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestUboot {
    #[arg(short = 'b', long, value_name = "BOARD")]
    pub board: String,

    #[arg(long, default_value = "linux", value_name = "GUEST")]
    pub guest: String,

    #[arg(long)]
    pub uboot_config: Option<PathBuf>,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available board names
    Ls,
}

pub struct Axvisor {
    app: AppContext,
    ctx: AxvisorContext,
}

impl From<&ArgsBuild> for AxvisorCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
            vmconfigs: args.vmconfigs.clone(),
        }
    }
}

impl Axvisor {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        let ctx = AxvisorContext::new()?;
        Ok(Self { app, ctx })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Defconfig(args) => self.defconfig(args),
            Command::Config(args) => self.config(args),
            Command::Image(args) => self.image(args).await,
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
        self.run_qemu_request(request).await
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

    fn defconfig(&mut self, args: ArgsDefconfig) -> anyhow::Result<()> {
        let workspace_root = self.app.workspace_root().to_path_buf();
        let axvisor_dir = self.app.axvisor_dir()?.to_path_buf();
        let path = config::write_defconfig(&workspace_root, &axvisor_dir, &args.board)?;
        println!("Generated {} for board {}", path.display(), args.board);
        Ok(())
    }

    fn config(&mut self, args: ArgsConfig) -> anyhow::Result<()> {
        match args.command {
            ConfigCommand::Ls => {
                for board in config::available_board_names(self.app.axvisor_dir()?)? {
                    println!("{board}");
                }
            }
        }
        Ok(())
    }

    async fn image(&self, args: image::Args) -> anyhow::Result<()> {
        image::run(args, &self.ctx).await
    }

    async fn test(&mut self, args: ArgsTest) -> anyhow::Result<()> {
        match args.command {
            TestCommand::Qemu(args) => self.test_qemu(args).await,
            TestCommand::Uboot(args) => self.test_uboot(args).await,
        }
    }

    async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        let (arch, target) = test_qemu::parse_axvisor_test_target(&args.target)?;

        println!(
            "running axvisor qemu tests for arch: {} (target: {})",
            arch, target
        );

        let vmconfig = match arch {
            "aarch64" => {
                qemu_test::prepare_linux_aarch64_guest_assets(&self.ctx)
                    .await?
                    .generated_vmconfig
            }
            "x86_64" => qemu_test::prepare_nimbos_x86_64_guest_vmconfig(&self.ctx).await?,
            _ => unreachable!(),
        };

        let request = self.prepare_request(
            Self::qemu_test_build_args(arch, vmconfig),
            None,
            None,
            SnapshotPersistence::Discard,
        )?;
        let qemu_config =
            qemu::default_qemu_config_template_path(&request.axvisor_dir, &request.arch);
        let shell = test_qemu::axvisor_test_shell_config(arch)?;
        let override_args = qemu_test::shell_autoinit_qemu_override_args(&request, &shell)?;

        self.app
            .qemu(
                build::load_cargo_config(&request)?,
                request.build_info_path,
                QemuRunConfig {
                    qemu_config: Some(qemu_config),
                    override_args,
                    ..Default::default()
                },
            )
            .await
            .with_context(|| "axvisor qemu test failed")
    }

    async fn test_uboot(&mut self, args: ArgsTestUboot) -> anyhow::Result<()> {
        let board = test_qemu::axvisor_uboot_board_config(&args.board, &args.guest)?;
        let explicit_uboot_config = args.uboot_config.clone();
        let uboot_config_summary = explicit_uboot_config
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "using ostool default search".to_string());

        if let Some(path) = explicit_uboot_config.as_ref()
            && !path.exists()
        {
            bail!(
                "missing explicit U-Boot config `{}` for axvisor board tests",
                path.display()
            );
        }

        println!(
            "running axvisor uboot test for board: {} guest: {} with vmconfig: {}",
            board.board, board.guest, board.vmconfig
        );

        let mut request = self.prepare_request(
            Self::uboot_test_build_args(board.build_config, board.vmconfig),
            None,
            explicit_uboot_config.clone(),
            SnapshotPersistence::Discard,
        )?;
        request.uboot_config = explicit_uboot_config;

        let cargo = build::load_cargo_config(&request)?;
        self.app
            .uboot(cargo, request.build_info_path, request.uboot_config)
            .await
            .with_context(|| {
                format!(
                    "axvisor uboot test failed for board `{}` guest `{}` (build_config={}, \
                     vmconfig={}, uboot_config={})",
                    board.board,
                    board.guest,
                    board.build_config,
                    board.vmconfig,
                    uboot_config_summary
                )
            })
    }

    fn qemu_test_build_args(arch: &str, vmconfig: PathBuf) -> AxvisorCliArgs {
        AxvisorCliArgs {
            config: None,
            arch: Some(arch.to_string()),
            target: None,
            plat_dyn: None,
            vmconfigs: vec![vmconfig],
        }
    }

    fn uboot_test_build_args(build_config: &str, vmconfig: &str) -> AxvisorCliArgs {
        AxvisorCliArgs {
            config: Some(PathBuf::from(build_config)),
            arch: None,
            target: None,
            plat_dyn: None,
            vmconfigs: vec![PathBuf::from(vmconfig)],
        }
    }

    fn prepare_request(
        &mut self,
        args: AxvisorCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedAxvisorRequest> {
        let (request, snapshot) =
            self.app
                .prepare_axvisor_request(args, qemu_config, uboot_config)?;
        if matches!(persistence, SnapshotPersistence::Store) {
            self.app.store_axvisor_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    fn qemu_run_config(request: &ResolvedAxvisorRequest) -> anyhow::Result<QemuRunConfig> {
        if let Some(path) = request.qemu_config.clone() {
            Ok(QemuRunConfig {
                qemu_config: Some(path),
                ..Default::default()
            })
        } else {
            qemu::default_qemu_run_config(request)
        }
    }

    async fn run_qemu_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        command_flow::run_qemu(
            &mut self.app,
            request,
            build::load_cargo_config,
            Self::qemu_run_config,
        )
        .await
    }

    async fn run_build_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        command_flow::run_uboot(&mut self.app, request, build::load_cargo_config).await
    }
}

impl Default for Axvisor {
    fn default() -> Self {
        Self::new().expect("failed to initialize Axvisor")
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::context::{ResolvedAxvisorRequest, workspace_member_dir, workspace_root_path};

    #[test]
    fn context_resolves_workspace_root() {
        let ctx = AxvisorContext::new().unwrap();
        assert_eq!(
            ctx.workspace_root(),
            workspace_root_path().unwrap().as_path()
        );
        assert_eq!(
            ctx.axvisor_dir(),
            workspace_member_dir(crate::axvisor::build::AXVISOR_PACKAGE)
                .unwrap()
                .as_path()
        );
    }

    #[test]
    fn command_parses_uboot() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "uboot",
            "--arch",
            "aarch64",
            "--uboot-config",
            "uboot.toml",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();

        match cli.command {
            Command::Uboot(args) => {
                assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.uboot_config, Some(PathBuf::from("uboot.toml")));
                assert_eq!(args.build.vmconfigs, vec![PathBuf::from("tmp/vm1.toml")]);
            }
            _ => panic!("expected uboot command"),
        }
    }

    #[test]
    fn command_parses_test_qemu() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axvisor", "test", "qemu", "--arch", "aarch64"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => assert_eq!(args.target, "aarch64"),
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_uboot() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axvisor",
            "test",
            "uboot",
            "-b",
            "roc-rk3568-pc",
            "--guest",
            "arceos",
            "--uboot-config",
            "uboot.toml",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Uboot(args) => {
                    assert_eq!(args.board, "roc-rk3568-pc");
                    assert_eq!(args.guest, "arceos");
                    assert_eq!(args.uboot_config, Some(PathBuf::from("uboot.toml")));
                }
                _ => panic!("expected uboot test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_uboot_with_default_guest() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axvisor", "test", "uboot", "-b", "roc-rk3568-pc"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Uboot(args) => assert_eq!(args.guest, "linux"),
                _ => panic!("expected uboot test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_build_and_qemu() {
        let build_config = "os/axvisor/.build.toml";
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let build_cli = Cli::try_parse_from([
            "axvisor",
            "build",
            "--config",
            build_config,
            "--arch",
            "aarch64",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();
        match build_cli.command {
            Command::Build(args) => {
                assert_eq!(args.config, Some(PathBuf::from(build_config)));
                assert_eq!(args.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.vmconfigs, vec![PathBuf::from("tmp/vm1.toml")]);
            }
            _ => panic!("expected build command"),
        }

        let qemu_cli = Cli::try_parse_from([
            "axvisor",
            "qemu",
            "--config",
            build_config,
            "--arch",
            "aarch64",
            "--qemu-config",
            "configs/qemu.toml",
            "--vmconfigs",
            "tmp/vm1.toml",
            "--vmconfigs",
            "tmp/vm2.toml",
        ])
        .unwrap();
        match qemu_cli.command {
            Command::Qemu(args) => {
                assert_eq!(args.build.config, Some(PathBuf::from(build_config)));
                assert_eq!(args.build.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.qemu_config, Some(PathBuf::from("configs/qemu.toml")));
                assert_eq!(
                    args.build.vmconfigs,
                    vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")]
                );
            }
            _ => panic!("expected qemu command"),
        }
    }

    #[test]
    fn default_qemu_run_config_lets_ostool_resolve_default_path() {
        let run_config = qemu::default_qemu_run_config(&ResolvedAxvisorRequest {
            package: "axvisor".to_string(),
            axvisor_dir: PathBuf::from("os/axvisor"),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            build_info_path: PathBuf::from("os/axvisor/.build-aarch64-unknown-none-softfloat.toml"),
            qemu_config: None,
            uboot_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert_eq!(run_config.qemu_config, None);
        assert!(run_config.default_args.args.is_some());
    }
}
