use std::path::PathBuf;

use clap::{Args, Subcommand};

use crate::context::{AppContext, BuildCliArgs, QemuRunConfig};

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
            Command::Build(args) => {
                self.build(args).await?;
            }
            Command::Qemu(args) => {
                self.qemu(args).await?;
            }
            Command::Uboot(args) => {
                self.uboot(args).await?;
            }
        }
        Ok(())
    }

    async fn build(&mut self, args: ArgsBuild) -> anyhow::Result<()> {
        let (request, snapshot) = self
            .app
            .prepare_arceos_request((&args).into(), None, None)?;
        self.app.store_arceos_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        self.app.build(cargo, request.build_info_path).await?;
        Ok(())
    }

    async fn qemu(&mut self, args: ArgsQemu) -> anyhow::Result<()> {
        let (request, snapshot) = self.app.prepare_arceos_request(
            (&args.build).into(),
            args.qemu_config.clone(),
            None,
        )?;
        self.app.store_arceos_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        self.app
            .qemu(
                cargo,
                request.build_info_path,
                QemuRunConfig {
                    qemu_config: request.qemu_config,
                    ..Default::default()
                },
            )
            .await?;
        Ok(())
    }

    async fn uboot(&mut self, args: ArgsUboot) -> anyhow::Result<()> {
        let (request, snapshot) = self.app.prepare_arceos_request(
            (&args.build).into(),
            None,
            args.uboot_config.clone(),
        )?;
        self.app.store_arceos_snapshot(&snapshot)?;

        let cargo = build::load_cargo_config(&request)?;
        self.app
            .uboot(cargo, request.build_info_path, request.uboot_config)
            .await?;
        Ok(())
    }
}

impl Default for ArceOS {
    fn default() -> Self {
        Self::new().expect("failed to initialize ArceOS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_args_convert_to_cli_args() {
        let args = ArgsBuild {
            config: None,
            package: Some("arceos-helloworld".to_string()),
            target: Some("aarch64-unknown-none-softfloat".to_string()),
            plat_dyn: Some(true),
        };

        let cli_args = BuildCliArgs::from(&args);

        assert_eq!(
            cli_args,
            BuildCliArgs {
                config: None,
                package: Some("arceos-helloworld".to_string()),
                target: Some("aarch64-unknown-none-softfloat".to_string()),
                plat_dyn: Some(true),
            }
        );
    }
}
