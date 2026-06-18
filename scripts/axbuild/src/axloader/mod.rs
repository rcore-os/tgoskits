use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::{
    axvisor::build,
    context::{
        AppContext, AxvisorCliArgs, AxvisorRequestPaths, ResolvedAxvisorRequest,
        SnapshotPersistence,
    },
};

pub mod test;

/// Axloader host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Build Axloader
    Build(ArgsBuild),
    /// Run Axloader test suites
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

    #[arg(long, value_name = "CPUS")]
    pub smp: Option<usize>,

    #[arg(long)]
    pub debug: bool,

    #[arg(long)]
    pub vmconfigs: Vec<PathBuf>,
}

#[derive(Args)]
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run Axloader QEMU test suite
    Qemu(ArgsTestQemu),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(
        long,
        value_name = "ARCH",
        required_unless_present_any = ["target", "list"],
        help = "Axloader architecture to test"
    )]
    pub arch: Option<String>,
    #[arg(
        short = 't',
        long,
        value_name = "TARGET",
        required_unless_present_any = ["arch", "list"],
        help = "Axloader target triple to test"
    )]
    pub target: Option<String>,
    #[arg(
        short = 'g',
        long = "test-group",
        value_name = "GROUP",
        help = "Run Axloader QEMU test cases from one test group"
    )]
    pub test_group: Option<String>,
    #[arg(
        short = 'c',
        long = "test-case",
        value_name = "CASE",
        help = "Run only one Axloader QEMU test case"
    )]
    pub test_case: Option<String>,
    #[arg(short = 'l', long, help = "List discovered Axloader QEMU test cases")]
    pub list: bool,
}

pub struct Axloader {
    pub(super) app: AppContext,
}

impl From<&ArgsBuild> for AxvisorCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            arch: args.arch.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
            smp: args.smp,
            debug: args.debug,
            vmconfigs: args.vmconfigs.clone(),
        }
    }
}

impl Axloader {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Test(args) => self.test(args).await,
        }
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let request = self.prepare_request((&args).into(), None, SnapshotPersistence::Store)?;
        self.run_build_request(request).await
    }

    async fn test(&mut self, args: ArgsTest) -> anyhow::Result<()> {
        test::test(self, args).await
    }

    pub(super) fn prepare_request(
        &mut self,
        args: AxvisorCliArgs,
        qemu_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedAxvisorRequest> {
        let axvisor_dir = self
            .app
            .workspace_member_dir(build::AXVISOR_PACKAGE)?
            .to_path_buf();
        let (request, snapshot) = self.app.prepare_axloader_request(
            args,
            AxvisorRequestPaths {
                package: build::AXVISOR_PACKAGE.to_string(),
                axvisor_dir,
                load_config_target: build::load_target_from_build_config,
                resolve_build_info_path: build::resolve_build_info_path,
            },
            qemu_config,
        )?;
        if persistence.should_store() {
            self.app.store_axloader_snapshot(&snapshot)?;
        }
        Ok(request)
    }

    async fn run_build_request(&mut self, request: ResolvedAxvisorRequest) -> anyhow::Result<()> {
        self.app.set_debug_mode(request.debug)?;
        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn command_parses_build() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axloader",
            "build",
            "--config",
            "os/axvisor/.build.toml",
            "--arch",
            "aarch64",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();

        match cli.command {
            Command::Build(args) => {
                assert_eq!(args.config, Some(PathBuf::from("os/axvisor/.build.toml")));
                assert_eq!(args.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.vmconfigs, vec![PathBuf::from("tmp/vm1.toml")]);
            }
            _ => panic!("expected build command"),
        }
    }

    #[test]
    fn command_parses_test_qemu() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from([
            "axloader", "test", "qemu", "--arch", "aarch64", "-g", "normal", "-c", "smoke",
        ])
        .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert_eq!(args.arch.as_deref(), Some("aarch64"));
                    assert_eq!(args.target, None);
                    assert_eq!(args.test_group.as_deref(), Some("normal"));
                    assert_eq!(args.test_case.as_deref(), Some("smoke"));
                    assert!(!args.list);
                }
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_qemu_list_without_arch() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axloader", "test", "qemu", "--list"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => {
                    assert!(args.arch.is_none());
                    assert!(args.target.is_none());
                    assert!(args.list);
                }
            },
            _ => panic!("expected test command"),
        }
    }
}
