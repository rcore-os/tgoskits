use std::path::PathBuf;

use anyhow::Context;
use clap::{Args, Subcommand};

use crate::{
    command_flow::{self, SnapshotPersistence},
    context::{AppContext, BuildCliArgs, QemuRunConfig, ResolvedBuildRequest},
    test_qemu,
};

pub mod build;

/// ArceOS subcommands
#[derive(Subcommand)]
pub enum Command {
    /// Build ArceOS application
    Build(ArgsBuild),
    /// Build and run ArceOS application
    Qemu(ArgsQemu),

    /// Build and run ArceOS application with U-Boot
    Uboot(ArgsUboot),
    /// Run ArceOS test suites
    Test(ArgsTest),
}

#[derive(Args)]
pub struct ArgsBuild {
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    #[arg(short, long)]
    pub package: Option<String>,
    #[arg(short, long)]
    pub target: Option<String>,
    #[arg(long = "plat_dyn", alias = "plat-dyn")]
    pub plat_dyn: Option<bool>,
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
pub struct ArgsTest {
    #[command(subcommand)]
    pub command: TestCommand,
}

#[derive(Subcommand)]
pub enum TestCommand {
    /// Run ArceOS QEMU test suites
    Qemu(ArgsTestQemu),
    /// Reserved ArceOS U-Boot test suite entrypoint
    Uboot(ArgsTestUboot),
}

#[derive(Args, Debug, Clone)]
pub struct ArgsTestQemu {
    #[arg(long)]
    pub target: String,
}

#[derive(Args, Debug, Clone, Default)]
pub struct ArgsTestUboot;

pub struct ArceOS {
    app: AppContext,
}

impl From<&ArgsBuild> for BuildCliArgs {
    fn from(args: &ArgsBuild) -> Self {
        Self {
            config: args.config.clone(),
            package: args.package.clone(),
            target: args.target.clone(),
            plat_dyn: args.plat_dyn,
        }
    }
}

impl ArceOS {
    pub fn new() -> anyhow::Result<Self> {
        let app = AppContext::new()?;
        Ok(Self { app })
    }

    pub async fn execute(&mut self, command: Command) -> anyhow::Result<()> {
        match command {
            Command::Build(args) => self.build(args).await,
            Command::Qemu(args) => self.qemu(args).await,
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

    async fn test(&mut self, args: ArgsTest) -> anyhow::Result<()> {
        match args.command {
            TestCommand::Qemu(args) => self.test_qemu(args).await,
            TestCommand::Uboot(args) => self.test_uboot(args).await,
        }
    }

    async fn test_qemu(&mut self, args: ArgsTestQemu) -> anyhow::Result<()> {
        let target = test_qemu::validate_arceos_target(&args.target)?;
        let mut failed = Vec::new();

        println!(
            "running arceos qemu tests for {} package(s) on target: {}",
            test_qemu::ARCEOS_TEST_PACKAGES.len(),
            target
        );

        for (index, package) in test_qemu::ARCEOS_TEST_PACKAGES.iter().enumerate() {
            println!(
                "[{}/{}] arceos qemu {}",
                index + 1,
                test_qemu::ARCEOS_TEST_PACKAGES.len(),
                package
            );
            let request = self.prepare_request(
                Self::test_build_args(package, target),
                None,
                None,
                SnapshotPersistence::Discard,
            )?;
            match self
                .run_qemu_request(request)
                .await
                .with_context(|| format!("arceos qemu test failed for package `{package}`"))
            {
                Ok(()) => println!("ok: {}", package),
                Err(err) => {
                    eprintln!("failed: {}: {:#}", package, err);
                    failed.push((*package).to_string());
                }
            }
        }

        test_qemu::finalize_qemu_test_run("arceos", &failed)
    }

    async fn test_uboot(&mut self, _args: ArgsTestUboot) -> anyhow::Result<()> {
        test_qemu::unsupported_uboot_test_command("arceos")
    }

    fn prepare_request(
        &self,
        args: BuildCliArgs,
        qemu_config: Option<PathBuf>,
        uboot_config: Option<PathBuf>,
        persistence: SnapshotPersistence,
    ) -> anyhow::Result<ResolvedBuildRequest> {
        command_flow::resolve_request(
            persistence,
            || {
                self.app
                    .prepare_arceos_request(args, qemu_config, uboot_config)
            },
            |snapshot| self.app.store_arceos_snapshot(snapshot),
        )
    }

    fn test_build_args(package: &str, target: &str) -> BuildCliArgs {
        BuildCliArgs {
            config: None,
            package: Some(package.to_string()),
            target: Some(target.to_string()),
            plat_dyn: None,
        }
    }

    fn qemu_run_config(request: &ResolvedBuildRequest) -> anyhow::Result<QemuRunConfig> {
        Ok(QemuRunConfig {
            qemu_config: request.qemu_config.clone(),
            ..Default::default()
        })
    }

    async fn run_qemu_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        command_flow::run_qemu(
            &mut self.app,
            request,
            build::load_cargo_config,
            Self::qemu_run_config,
        )
        .await
    }

    async fn run_build_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        command_flow::run_build(&mut self.app, request, build::load_cargo_config).await
    }

    async fn run_uboot_request(&mut self, request: ResolvedBuildRequest) -> anyhow::Result<()> {
        command_flow::run_uboot(&mut self.app, request, build::load_cargo_config).await
    }
}

impl Default for ArceOS {
    fn default() -> Self {
        Self::new().expect("failed to initialize ArceOS")
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

        let cli =
            Cli::try_parse_from(["arceos", "test", "qemu", "--target", "x86_64-unknown-none"])
                .unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Qemu(args) => assert_eq!(args.target, "x86_64-unknown-none"),
                _ => panic!("expected qemu test command"),
            },
            _ => panic!("expected test command"),
        }
    }

    #[test]
    fn command_parses_test_uboot() {
        #[derive(Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["arceos", "test", "uboot"]).unwrap();

        match cli.command {
            Command::Test(args) => match args.command {
                TestCommand::Uboot(_) => {}
                _ => panic!("expected uboot test command"),
            },
            _ => panic!("expected test command"),
        }
    }
}
