use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::{
    axvisor::context::AxvisorContext,
    context::{AppContext, AxvisorCliArgs, QemuRunConfig},
};

pub mod board;
pub mod build;
pub mod context;
pub mod image;
pub mod qemu_test;

/// Axvisor host-side commands
#[derive(Subcommand)]
pub enum Command {
    /// Build Axvisor
    Build(ArgsBuild),
    /// Build and run Axvisor in QEMU
    Qemu(ArgsQemu),
    /// Guest image management
    Image(image::Args),
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
            Command::Build(args) => {
                self.build(args).await?;
            }
            Command::Qemu(args) => {
                self.qemu(args).await?;
            }
            Command::Image(args) => {
                self.image(args).await?;
            }
        }
        Ok(())
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let (request, snapshot) = self.app.prepare_axvisor_request((&args).into(), None)?;
        self.app.store_axvisor_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await?;
        Ok(())
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let (request, snapshot) = self
            .app
            .prepare_axvisor_request((&args.build).into(), args.qemu_config.clone())?;
        self.app.store_axvisor_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        let qemu = if let Some(path) = request.qemu_config.clone() {
            QemuRunConfig {
                qemu_config: Some(path),
                ..Default::default()
            }
        } else {
            build::default_qemu_run_config(&request)?
        };
        self.app.qemu(cargo, request.build_info_path, qemu).await?;
        Ok(())
    }

    async fn image(&self, args: image::Args) -> anyhow::Result<()> {
        image::run(args, &self.ctx).await
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
    use crate::context::{ResolvedAxvisorRequest, workspace_root_path};

    #[test]
    fn context_resolves_workspace_root() {
        let ctx = AxvisorContext::new().unwrap();
        assert_eq!(
            ctx.workspace_root(),
            workspace_root_path().unwrap().as_path()
        );
    }

    #[test]
    fn command_parses_image_ls() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axvisor", "image", "ls"]).unwrap();

        match cli.command {
            Command::Image(_) => {}
            _ => panic!("expected image command"),
        }
    }

    #[test]
    fn command_parses_image_pull() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let cli = Cli::try_parse_from(["axvisor", "image", "pull", "linux"]).unwrap();

        match cli.command {
            Command::Image(_) => {}
            _ => panic!("expected image command"),
        }
    }

    #[test]
    fn build_args_convert_to_cli_args() {
        let args = ArgsBuild {
            config: Some(PathBuf::from("os/axvisor/.build.toml")),
            arch: Some("aarch64".to_string()),
            target: Some("aarch64-unknown-none-softfloat".to_string()),
            plat_dyn: Some(false),
            vmconfigs: vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")],
        };

        let cli_args = AxvisorCliArgs::from(&args);

        assert_eq!(
            cli_args,
            AxvisorCliArgs {
                config: Some(PathBuf::from("os/axvisor/.build.toml")),
                arch: Some("aarch64".to_string()),
                target: Some("aarch64-unknown-none-softfloat".to_string()),
                plat_dyn: Some(false),
                vmconfigs: vec![PathBuf::from("tmp/vm1.toml"), PathBuf::from("tmp/vm2.toml")],
            }
        );
    }

    #[test]
    fn command_parses_build_and_qemu() {
        #[derive(clap::Parser)]
        struct Cli {
            #[command(subcommand)]
            command: Command,
        }

        let build_cli = Cli::try_parse_from([
            "axvisor",
            "build",
            "--config",
            "os/axvisor/.build.toml",
            "--arch",
            "aarch64",
            "--vmconfigs",
            "tmp/vm1.toml",
        ])
        .unwrap();
        match build_cli.command {
            Command::Build(args) => {
                assert_eq!(args.config, Some(PathBuf::from("os/axvisor/.build.toml")));
                assert_eq!(args.arch.as_deref(), Some("aarch64"));
                assert_eq!(args.vmconfigs, vec![PathBuf::from("tmp/vm1.toml")]);
            }
            _ => panic!("expected build command"),
        }

        let qemu_cli = Cli::try_parse_from([
            "axvisor",
            "qemu",
            "--config",
            "os/axvisor/.build.toml",
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
                assert_eq!(
                    args.build.config,
                    Some(PathBuf::from("os/axvisor/.build.toml"))
                );
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
        let run_config = build::default_qemu_run_config(&ResolvedAxvisorRequest {
            package: "axvisor".to_string(),
            arch: "aarch64".to_string(),
            target: "aarch64-unknown-none-softfloat".to_string(),
            plat_dyn: None,
            build_info_path: PathBuf::from("os/axvisor/.build-aarch64-unknown-none-softfloat.toml"),
            qemu_config: None,
            vmconfigs: vec![],
        })
        .unwrap();

        assert_eq!(run_config.qemu_config, None);
        assert!(run_config.default_args.args.is_some());
    }
}
