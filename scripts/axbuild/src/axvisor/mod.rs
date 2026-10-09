use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use ostool::{
    board::{RunBoardOptions, config::BoardRunConfig},
    build::config::Cargo,
};

use crate::context::{
    AppContext, AxvisorCliArgs, AxvisorRequestPaths, ResolvedAxvisorRequest, SnapshotPersistence,
};

pub mod board;
pub mod build;
mod bundle;
pub mod config;
pub mod rootfs;
pub mod test;

/// Axvisor host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Build Axvisor
    Build(ArgsBuild),
    /// Build and run Axvisor in QEMU
    Qemu(ArgsQemu),
    /// Build and run Axvisor on a remote board
    Board(ArgsBoard),
    /// Run Axvisor test suites
    Test(ArgsTest),
    /// Build and run Axvisor with U-Boot
    Uboot(ArgsUboot),
    /// Generate a default board config
    Defconfig(ArgsDefconfig),
    /// Board config helpers
    Config(ArgsConfig),
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

    #[arg(long = "vmconfig", alias = "vmconfigs")]
    pub vmconfigs: Vec<PathBuf>,
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
    /// Run Axvisor remote board test suite
    Board(ArgsTestBoard),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present_any = ["target", "list"],
        help = "Axvisor architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present_any = ["arch", "list"],
        help = "Axvisor target triple to test"
    )]
    pub target: Option<String>,
    #[arg(
        short = 'g',
        long = "test-group",
        value_name = "GROUP",
        help = "Run Axvisor QEMU test cases from one test group"
    )]
    pub test_group: Option<String>,
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        value_delimiter = ',',
        help = "Run selected Axvisor QEMU cases; repeat or separate with commas"
    )]
    pub test_case: Vec<String>,
    #[arg(short = 'l', long, help = "List discovered Axvisor QEMU test cases")]
    pub list: bool,
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

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestBoard {
    #[arg(
        short = 'g',
        long = "test-group",
        value_name = "GROUP",
        help = "Run Axvisor board test cases from one test group"
    )]
    pub test_group: Option<String>,

    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        value_delimiter = ',',
        help = "Run one or more Axvisor board test cases"
    )]
    pub test_case: Vec<String>,

    #[arg(
        long,
        value_name = "BOARD",
        value_delimiter = ',',
        help = "Run all Axvisor board test cases for one or more boards"
    )]
    pub board: Vec<String>,

    #[arg(short = 'b', long = "board-type", value_name = "BOARD_TYPE")]
    pub board_type: Option<String>,

    #[arg(long)]
    pub server: Option<String>,

    #[arg(long)]
    pub port: Option<u16>,

    #[arg(short = 'l', long, help = "List discovered Axvisor board test cases")]
    pub list: bool,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// List available board names
    Ls,
    /// Edit one guest configuration with menuconfig
    Vm {
        /// Guest configuration file to edit
        guest_config: PathBuf,
    },
}

pub struct Axvisor {
    app: AppContext,
}

impl From<&ArgsBuild> for AxvisorCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            smp: args.smp,
            debug: args.debug,
            vmconfigs: args.vmconfigs.clone(),
        }
    }
}

impl Axvisor {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
            Command::Uboot(args) => self.uboot(args).await,
            Command::Board(args) => self.board(args).await,
            Command::Defconfig(args) => self.defconfig(args),
            Command::Config(args) => self.config(args).await,
            Command::Test(args) => self.test(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request =
            self.prepare_request((&args).into(), None, None, SnapshotPersistence::Store)?;
        self.run_build_request(request).await
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        rootfs::qemu(self, args).await
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
        let mut request =
            self.prepare_request((&args.build).into(), None, None, SnapshotPersistence::Store)?;
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        let mut board_config = self
            .load_board_config(&cargo, args.board_config.as_deref())
            .await?;
        // Board-only handoffs may resolve guest images to paths published by
        // the board root filesystem (for example `/linux/...`).
        self.prepare_guest_payload(&mut request, &mut board_config.boot, true)
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

    fn defconfig(&mut self, args: ArgsDefconfig) -> anyhow::Result<()> {
        let workspace_root = self.app.workspace_root().to_path_buf();
        let axvisor_dir = self
            .app
            .workspace_member_dir(build::AXVISOR_PACKAGE)?
            .to_path_buf();
        let path = config::write_defconfig(&workspace_root, &axvisor_dir, &args.board)?;
        println!("Generated {} for board {}", path.display(), args.board);
        Ok(())
    }

    async fn config(&mut self, args: ArgsConfig) -> anyhow::Result<()> {
        match args.command {
            ConfigCommand::Ls => {
                for board in config::available_board_names(
                    self.app.workspace_member_dir(build::AXVISOR_PACKAGE)?,
                )? {
                    println!("{board}");
                }
            }
            ConfigCommand::Vm { guest_config } => {
                let _ = jkconfig::run::<axvmconfig::GuestConfig>(guest_config, true, &[]).await?;
            }
        }
        Ok(())
    }

    async fn test(&mut self, args: ArgsTest) -> anyhow::Result<()> {
        test::test(self, args).await
    }

    pub(super) fn prepare_request(
        &mut self,
        args: AxvisorCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedAxvisorRequest> {
        let axvisor_dir = self
            .app
            .workspace_member_dir(build::AXVISOR_PACKAGE)?
            .to_path_buf();
        let (request, snapshot) = self.app.prepare_axvisor_request(
            args,
            AxvisorRequestPaths {
                package: build::AXVISOR_PACKAGE.to_string(),
                axvisor_dir,
                load_config_target: build::load_target_from_build_config,
                resolve_build_info_path: build::resolve_build_info_path,
            },
            qemu_config,
            uboot_config,
        )?;
        if persistence.should_store() {
            self.app.store_axvisor_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    async fn load_uboot_config(
        &mut self,
        request: &ResolvedAxvisorRequest,
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

    async fn run_build_request(
        &mut self,
        mut request: ResolvedAxvisorRequest,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        self.app
            .build(cargo, request.build_info_path.clone())
            .await?;
        self.prepare_guest_payload(
            &mut request,
            &mut ostool::BootPayloadConfig::default(),
            false,
        )
        .await
    }

    async fn run_uboot_request(
        &mut self,
        mut request: ResolvedAxvisorRequest,
    ) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request, self.app.workspace_context())?;
        let mut uboot = match self.load_uboot_config(&request, &cargo).await? {
            Some(config) => config,
            None => self.app.ensure_uboot_config_for_cargo(&cargo).await?,
        };
        self.prepare_guest_payload(&mut request, &mut uboot.boot, true)
            .await?;
        self.app
            .uboot(cargo, request.build_info_path, Some(uboot))
            .await
    }

    pub(super) async fn prepare_guest_payload(
        &mut self,
        request: &mut ResolvedAxvisorRequest,
        boot: &mut ostool::BootPayloadConfig,
        allow_external_assets: bool,
    ) -> anyhow::Result<()> {
        request.vmconfigs = build::load_vmconfigs(request, self.app.workspace_context())?;
        rootfs::ensure_guest_image_bundles(
            request,
            self.app.workspace_root(),
            self.app.target_dir(),
        )
        .await?;
        let output = self
            .app
            .target_dir()
            .join("axbuild/axvisor/host-initramfs")
            .join(format!("{}.cpio", request.arch));
        bundle::attach_with_external_assets(
            &request.vmconfigs,
            false,
            &output,
            &mut boot.initramfs,
            allow_external_assets,
        )
    }
}

fn default_qemu_config_template_path(axvisor_dir: &Path, arch: &str) -> PathBuf {
    axvisor_dir.join(format!("configs/qemu/qemu-{arch}.toml"))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct BoardCli {
        #[command(flatten)]
        board: ArgsTestBoard,
    }

    #[test]
    fn board_case_selector_accepts_repeated_and_comma_separated_values() {
        let cli =
            BoardCli::try_parse_from(["test", "--test-case", "smoke,direct", "--test-case", "pci"])
                .unwrap();

        assert_eq!(cli.board.test_case, ["smoke", "direct", "pci"]);
    }
}
